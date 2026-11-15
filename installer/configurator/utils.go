// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

package main

import (
	"archive/tar"
	"archive/zip"
	"compress/gzip"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"

	log "github.com/sirupsen/logrus"
)

// Proxy config paths are frequently symlinks (distro alternatives, or an admin
// keeping the real file elsewhere), and neither proxy reports the link target.
// Every write goes through the resolved path so the link and its target survive
// instead of being replaced by a regular file.
func resolveConfigPath(path string) (string, error) {
	resolved, err := filepath.EvalSymlinks(path)
	if err != nil {
		return "", fmt.Errorf("failed to resolve config path '%s': %w", path, err)
	}

	if resolved != path {
		log.Debug("Config path '", path, "' resolves to '", resolved, "'")
	}

	return resolved, nil
}

// Writes a candidate config the proxy can be asked to validate, next to the real
// config so relative includes resolve exactly as they do for the config itself.
// The candidate inherits the config's mode, so one derived from an 0600 config is
// not left world-readable while it sits on disk.
func writeConfigCandidate(configPath string, content []byte) (string, error) {
	resolved, err := resolveConfigPath(configPath)
	if err != nil {
		return "", err
	}

	info, err := os.Stat(resolved)
	if err != nil {
		return "", fmt.Errorf("failed to stat config '%s': %w", resolved, err)
	}

	candidatePath := resolved + ".datadog"
	if err := os.WriteFile(candidatePath, content, info.Mode().Perm()); err != nil {
		return "", fmt.Errorf("failed to write candidate config '%s': %w", candidatePath, err)
	}

	// os.WriteFile leaves the mode alone when the file already exists, so without
	// this a candidate left behind by an earlier run could keep a laxer mode.
	if err := os.Chmod(candidatePath, info.Mode().Perm()); err != nil {
		return "", fmt.Errorf("failed to set the mode of candidate config '%s': %w", candidatePath, err)
	}

	return candidatePath, nil
}

// Backs the config up to <resolved>+backupSuffix and then overwrites the config
// itself, returning the backup path.
//
// The config is rewritten in place rather than renamed over: keeping the inode
// keeps its mode, ownership, xattrs (SELinux labels among them), ACLs, hard links
// and symlink identity, all of which a rename discards. A failed write is rolled
// back from the backup, so the config is either fully updated or untouched.
func replaceConfigFile(configPath, backupSuffix string, content []byte) (string, error) {
	resolved, err := resolveConfigPath(configPath)
	if err != nil {
		return "", err
	}

	info, err := os.Stat(resolved)
	if err != nil {
		return "", fmt.Errorf("failed to stat config '%s': %w", resolved, err)
	}

	backupPath := resolved + backupSuffix
	if err := copyFilePreservingOwner(resolved, backupPath, info); err != nil {
		return "", fmt.Errorf("failed to back up config '%s' to '%s': %w", resolved, backupPath, err)
	}

	log.Debug("Backed up config '", resolved, "' to '", backupPath, "'")

	if err := overwriteFileInPlace(resolved, content); err != nil {
		if restoreErr := restoreConfigFile(resolved, backupPath); restoreErr != nil {
			return backupPath, fmt.Errorf("failed to write config '%s' (%v), and restoring it from '%s' failed too: %w", resolved, err, backupPath, restoreErr)
		}

		return "", fmt.Errorf("failed to write config '%s', restored the original: %w", resolved, err)
	}

	return backupPath, nil
}

// Puts a backup taken by replaceConfigFile back, in place for the same reasons,
// and drops the backup afterwards so a rolled-back change leaves nothing behind.
func restoreConfigFile(configPath, backupPath string) error {
	resolved, err := resolveConfigPath(configPath)
	if err != nil {
		return err
	}

	content, err := os.ReadFile(backupPath)
	if err != nil {
		return fmt.Errorf("failed to read backup '%s': %w", backupPath, err)
	}

	if err := overwriteFileInPlace(resolved, content); err != nil {
		return fmt.Errorf("failed to restore config '%s' from '%s': %w", resolved, backupPath, err)
	}

	if err := os.Remove(backupPath); err != nil {
		return fmt.Errorf("failed to remove backup '%s' after restoring it: %w", backupPath, err)
	}

	log.Info("Restored config '", resolved, "' from its backup")

	return nil
}

// Truncates an existing file and writes content into it, keeping the inode.
// O_CREATE is deliberately absent: the file must already exist, which makes the
// mode argument inert and guarantees no new file is created by accident.
func overwriteFileInPlace(path string, content []byte) error {
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_TRUNC, 0)
	if err != nil {
		return err
	}

	if _, err := file.Write(content); err != nil {
		file.Close()
		return err
	}

	if err := file.Sync(); err != nil {
		file.Close()
		return err
	}

	return file.Close()
}

func copyFilePreservingOwner(srcPath, dstPath string, srcInfo os.FileInfo) error {
	content, err := os.ReadFile(srcPath)
	if err != nil {
		return err
	}

	dst, err := os.OpenFile(dstPath, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, srcInfo.Mode().Perm())
	if err != nil {
		return err
	}

	if _, err := dst.Write(content); err != nil {
		dst.Close()
		return err
	}

	if err := dst.Sync(); err != nil {
		dst.Close()
		return err
	}

	if err := dst.Close(); err != nil {
		return err
	}

	// O_CREATE honours the umask, and an existing file keeps its own mode
	// entirely, so the mode has to be set explicitly either way.
	if err := os.Chmod(dstPath, srcInfo.Mode().Perm()); err != nil {
		return err
	}

	chownLike(dstPath, srcInfo)

	return nil
}

// Gives dst the same owner and group as the file the info describes. Best effort
// by design: an unprivileged run cannot chown, and that must not fail the
// operation that asked for the copy.
func chownLike(dst string, info os.FileInfo) {
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok {
		log.Debug("Ownership is not reported on this platform, leaving the owner of '", dst, "' as created")
		return
	}

	if err := os.Chown(dst, int(stat.Uid), int(stat.Gid)); err != nil {
		log.Debug("Could not set the owner of '", dst, "': ", err)
	}
}

// Renames dir to a hidden sibling so a proxy can be config-tested with the
// directory unavailable, which is what proves nothing still references it.
//
// A sibling rather than a temporary directory, so the rename can never cross a
// filesystem boundary. Both returned functions are safe to call any number of
// times and no-op when dir never existed; restore also no-ops once remove has
// run, so it can be deferred unconditionally.
func moveDirAside(dir string) (restore func() error, remove func() error, err error) {
	noop := func() error { return nil }

	if _, err := os.Stat(dir); os.IsNotExist(err) {
		log.Debug("Directory '", dir, "' does not exist, nothing to move aside")
		return noop, noop, nil
	} else if err != nil {
		return nil, nil, fmt.Errorf("failed to stat '%s': %w", dir, err)
	}

	asidePath := filepath.Join(filepath.Dir(dir), "."+filepath.Base(dir)+".datadog-aside")
	if err := os.RemoveAll(asidePath); err != nil {
		return nil, nil, fmt.Errorf("failed to clear '%s' left over from an earlier run: %w", asidePath, err)
	}

	if err := os.Rename(dir, asidePath); err != nil {
		return nil, nil, fmt.Errorf("failed to move '%s' aside to '%s': %w", dir, asidePath, err)
	}

	log.Debug("Moved '", dir, "' aside to '", asidePath, "'")

	done := false

	restore = func() error {
		if done {
			return nil
		}
		done = true

		if err := os.Rename(asidePath, dir); err != nil {
			return fmt.Errorf("failed to move '%s' back to '%s': %w", asidePath, dir, err)
		}

		log.Debug("Moved '", asidePath, "' back to '", dir, "'")

		return nil
	}

	remove = func() error {
		if done {
			return nil
		}
		done = true

		return os.RemoveAll(asidePath)
	}

	return restore, remove, nil
}

func compareVersions(v1, v2 string) int {
	v1Segments := strings.Split(v1, ".")
	v2Segments := strings.Split(v2, ".")

	for i := 0; i < len(v1Segments) && i < len(v2Segments); i++ {
		v1digit, _ := strconv.Atoi(v1Segments[i])
		v2digit, _ := strconv.Atoi(v2Segments[i])
		if v1digit < v2digit {
			return -1
		}
		if v1digit > v2digit {
			return 1
		}
	}

	if len(v1Segments) < len(v2Segments) {
		return -1
	}
	if len(v1Segments) > len(v2Segments) {
		return 1
	}

	return 0
}

func extractTarGzFile(srcPath, destPath string) error {
	file, err := os.Open(srcPath)
	if err != nil {
		return err
	}
	defer file.Close()

	gzReader, err := gzip.NewReader(file)
	if err != nil {
		return err
	}
	defer gzReader.Close()

	tarReader := tar.NewReader(gzReader)

	for {
		header, err := tarReader.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			return err
		}

		dest := filepath.Join(destPath, header.Name)

		switch header.Typeflag {
		case tar.TypeReg:
			f, err := os.OpenFile(dest, os.O_CREATE|os.O_RDWR, os.FileMode(header.Mode))
			if err != nil {
				return err
			}
			if _, err := io.Copy(f, tarReader); err != nil {
				f.Close()
				return err
			}
			f.Close()
		default:
			return NewInstallerError(InternalError, fmt.Errorf("unsupported file type to extract: %v", header.Typeflag))
		}
	}

	log.Debug("Extracted file '", srcPath, "' to destination '", destPath, "'")

	return nil
}

func unzipFile(srcPath, destPath string) error {
	log.Debug("Unzipping file ", srcPath, " to ", destPath)

	reader, err := zip.OpenReader(srcPath)
	if err != nil {
		return err
	}
	defer reader.Close()

	for _, file := range reader.File {
		if err := copySingleZipFile(file, destPath); err != nil {
			return err
		}
	}
	return nil
}

func copySingleZipFile(file *zip.File, destPath string) error {
	filePath := filepath.Join(destPath, file.Name)
	if file.FileInfo().IsDir() {
		if err := os.MkdirAll(filePath, os.ModePerm); err != nil {
			return err
		}
		return nil
	}

	if err := os.MkdirAll(filepath.Dir(filePath), os.ModePerm); err != nil {
		return err
	}

	destFile, err := os.Create(filePath)
	if err != nil {
		return err
	}
	defer destFile.Close()

	srcFile, err := file.Open()
	if err != nil {
		return err
	}
	defer srcFile.Close()

	if _, err := io.Copy(destFile, srcFile); err != nil {
		return err
	}

	return nil
}

func keyValuePairsAsTags(pairs map[string]string) string {
	result := make([]string, 0, len(pairs))
	for key, value := range pairs {
		result = append(result, fmt.Sprintf("%s:%s", key, value))
	}
	return strings.Join(result, ", ")
}

func envBool(key string) bool {
	value := os.Getenv(key)
	value = strings.TrimSpace(strings.ToLower(value))
	return value != "false" && value != "0"
}

func ensureProtocol(url string) string {
	if !strings.HasPrefix(url, "http://") && !strings.HasPrefix(url, "https://") {
		return "http://" + url
	}
	return url
}

func commandExists(cmd string) bool {
	_, err := exec.LookPath(cmd)
	return err == nil
}

func verifySignatureGPG(moduleFile, signatureFile, publicKeyFile string) error {
	importCmd := exec.Command("gpg", "--import", publicKeyFile)
	if output, err := importCmd.CombinedOutput(); err != nil {
		return NewInstallerError(InternalError, fmt.Errorf("failed to import public key: %v\nOutput: %s", err, output))
	}

	log.Debug("Imported public key ", publicKeyFile)

	verifyCmd := exec.Command("gpg", "--verify", signatureFile, moduleFile)
	if output, err := verifyCmd.CombinedOutput(); err != nil {
		return NewInstallerError(InternalError, fmt.Errorf("signature verification failed: %v\nOutput: %s", err, output))
	}

	log.Debug("Verified signature ", signatureFile, " for module ", moduleFile)

	return nil
}

func downloadFile(url string) ([]byte, error) {
	log.Debug("Downloading file: ", url)

	resp, err := http.Get(url)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	respBytes, err := io.ReadAll(resp.Body)
	if err != nil {
		return respBytes, err
	}

	if resp.StatusCode < 200 || resp.StatusCode >= 400 {
		return respBytes, NewInstallerError(InternalError, fmt.Errorf("failed to download file with status code %s: %s", resp.Status, string(respBytes)))
	}

	return respBytes, nil
}
