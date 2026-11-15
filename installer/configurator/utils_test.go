// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

package main

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
	"gotest.tools/assert"
)

// Ownership is deliberately untested: 'go test' runs unprivileged, so chown is
// best effort and chownLike is written to no-op rather than fail.

// A temporary directory with symlinks already resolved, so paths the helpers
// return through resolveConfigPath can be compared against paths built here.
// macOS puts the temporary directory behind /var -> /private/var.
func testConfigDir(t *testing.T) string {
	t.Helper()

	dir, err := filepath.EvalSymlinks(t.TempDir())
	require.NoError(t, err, "Failed to resolve the temporary directory")

	return dir
}

// Writes a config file with the given mode and returns its path.
func writeTestConfig(t *testing.T, name, content string, perm os.FileMode) string {
	t.Helper()

	path := filepath.Join(testConfigDir(t), name)
	require.NoError(t, os.WriteFile(path, []byte(content), perm), "Failed to write the test config")
	require.NoError(t, os.Chmod(path, perm), "Failed to set the mode of the test config")

	return path
}

func TestReplaceConfigFilePreservesTheFile(t *testing.T) {
	const original = "Listen 80\n"
	const replacement = "Listen 8080\n"

	path := writeTestConfig(t, "httpd.conf", original, 0600)

	before, err := os.Stat(path)
	require.NoError(t, err, "Failed to stat the config")

	backupPath, err := replaceConfigFile(path, ".bak", []byte(replacement))
	require.NoError(t, err, "Failed to replace the config")

	after, err := os.Stat(path)
	require.NoError(t, err, "Failed to stat the config after replacement")

	// The same inode, which is what carries the mode, ownership, xattrs (SELinux
	// labels among them), ACLs and any hard links. A rename over the config would
	// have replaced it with a fresh inode holding none of those.
	assert.Assert(t, os.SameFile(before, after), "The config was replaced rather than rewritten in place")
	assert.Equal(t, os.FileMode(0600), after.Mode().Perm(), "The config lost its mode")

	content, err := os.ReadFile(path)
	require.NoError(t, err, "Failed to read the config")
	assert.Equal(t, replacement, string(content), "The config was not updated")

	assert.Equal(t, path+".bak", backupPath, "The backup was not written beside the config")

	backup, err := os.ReadFile(backupPath)
	require.NoError(t, err, "Failed to read the backup")
	assert.Equal(t, original, string(backup), "The backup does not hold the original config")

	backupInfo, err := os.Stat(backupPath)
	require.NoError(t, err, "Failed to stat the backup")
	assert.Equal(t, os.FileMode(0600), backupInfo.Mode().Perm(), "The backup is more permissive than the config it holds")
}

func TestReplaceConfigFileFollowsSymlinks(t *testing.T) {
	const original = "Listen 80\n"
	const replacement = "Listen 8080\n"

	realPath := writeTestConfig(t, "httpd.conf.real", original, 0640)
	linkPath := filepath.Join(filepath.Dir(realPath), "httpd.conf")
	require.NoError(t, os.Symlink(realPath, linkPath), "Failed to create the config symlink")

	backupPath, err := replaceConfigFile(linkPath, ".bak", []byte(replacement))
	require.NoError(t, err, "Failed to replace a symlinked config")

	linkInfo, err := os.Lstat(linkPath)
	require.NoError(t, err, "Failed to lstat the config symlink")
	assert.Assert(t, linkInfo.Mode()&os.ModeSymlink != 0, "The symlink was replaced by a regular file")

	content, err := os.ReadFile(realPath)
	require.NoError(t, err, "Failed to read the real config")
	assert.Equal(t, replacement, string(content), "The symlink target was not updated")

	// Beside the real file, not beside the link, so the backup is not mistaken for
	// a second config the way a <link>.bak regular file would be.
	assert.Equal(t, realPath+".bak", backupPath, "The backup was not written beside the real config")
}

func TestReplaceConfigFileRefusesAMissingConfig(t *testing.T) {
	path := filepath.Join(testConfigDir(t), "httpd.conf")

	_, err := replaceConfigFile(path, ".bak", []byte("Listen 80\n"))
	require.Error(t, err, "Expected replacing a missing config to fail")

	_, err = os.Stat(path + ".bak")
	assert.Assert(t, os.IsNotExist(err), "A backup was created for a config that does not exist")
}

func TestRestoreConfigFile(t *testing.T) {
	const original = "Listen 80\n"

	path := writeTestConfig(t, "httpd.conf", original, 0600)

	before, err := os.Stat(path)
	require.NoError(t, err, "Failed to stat the config")

	backupPath, err := replaceConfigFile(path, ".uninstall.backup", []byte("Listen 8080\n"))
	require.NoError(t, err, "Failed to replace the config")

	require.NoError(t, restoreConfigFile(path, backupPath), "Failed to restore the config")

	content, err := os.ReadFile(path)
	require.NoError(t, err, "Failed to read the restored config")
	assert.Equal(t, original, string(content), "The config was not restored")

	after, err := os.Stat(path)
	require.NoError(t, err, "Failed to stat the restored config")
	assert.Assert(t, os.SameFile(before, after), "Restoring replaced the config rather than rewriting it in place")
	assert.Equal(t, os.FileMode(0600), after.Mode().Perm(), "The restored config lost its mode")

	// A rolled back change leaves nothing behind for an operator to puzzle over.
	_, err = os.Stat(backupPath)
	assert.Assert(t, os.IsNotExist(err), "The backup outlived the restore it was used for")
}

func TestWriteConfigCandidateInheritsTheConfigMode(t *testing.T) {
	path := writeTestConfig(t, "httpd.conf", "Listen 80\n", 0600)

	candidatePath, err := writeConfigCandidate(path, []byte("Listen 8080\n"))
	require.NoError(t, err, "Failed to write the candidate config")

	assert.Equal(t, path+".datadog", candidatePath, "The candidate was not written beside the config")

	info, err := os.Stat(candidatePath)
	require.NoError(t, err, "Failed to stat the candidate config")
	assert.Equal(t, os.FileMode(0600), info.Mode().Perm(), "A candidate derived from an 0600 config is world-readable")

	content, err := os.ReadFile(candidatePath)
	require.NoError(t, err, "Failed to read the candidate config")
	assert.Equal(t, "Listen 8080\n", string(content), "The candidate does not hold the new config")
}

// A candidate left behind by an earlier run must not keep a laxer mode than the
// config it is now derived from.
func TestWriteConfigCandidateTightensAnExistingCandidate(t *testing.T) {
	path := writeTestConfig(t, "httpd.conf", "Listen 80\n", 0600)
	require.NoError(t, os.WriteFile(path+".datadog", []byte("stale\n"), 0644), "Failed to write a stale candidate")
	require.NoError(t, os.Chmod(path+".datadog", 0644), "Failed to set the mode of the stale candidate")

	candidatePath, err := writeConfigCandidate(path, []byte("Listen 8080\n"))
	require.NoError(t, err, "Failed to write the candidate config")

	info, err := os.Stat(candidatePath)
	require.NoError(t, err, "Failed to stat the candidate config")
	assert.Equal(t, os.FileMode(0600), info.Mode().Perm(), "A stale candidate kept its laxer mode")
}

func TestMoveDirAside(t *testing.T) {
	t.Run("Restores the directory", func(t *testing.T) {
		dir := filepath.Join(t.TempDir(), "datadog-httpd")
		require.NoError(t, os.MkdirAll(dir, 0755), "Failed to create the directory")
		require.NoError(t, os.WriteFile(filepath.Join(dir, "datadog.conf"), []byte("x\n"), 0644), "Failed to populate the directory")

		restore, _, err := moveDirAside(dir)
		require.NoError(t, err, "Failed to move the directory aside")

		_, err = os.Stat(dir)
		assert.Assert(t, os.IsNotExist(err), "The directory is still in place")

		require.NoError(t, restore(), "Failed to restore the directory")

		content, err := os.ReadFile(filepath.Join(dir, "datadog.conf"))
		require.NoError(t, err, "Failed to read the restored directory")
		assert.Equal(t, "x\n", string(content), "The restored directory lost its content")
	})

	t.Run("Removes the directory, after which restore no-ops", func(t *testing.T) {
		dir := filepath.Join(t.TempDir(), "datadog-httpd")
		require.NoError(t, os.MkdirAll(dir, 0755), "Failed to create the directory")

		restore, remove, err := moveDirAside(dir)
		require.NoError(t, err, "Failed to move the directory aside")

		require.NoError(t, remove(), "Failed to remove the directory")

		// restore is deferred unconditionally by the callers, so it has to be
		// harmless once the directory has been removed on purpose.
		require.NoError(t, restore(), "Restoring a removed directory failed")

		_, err = os.Stat(dir)
		assert.Assert(t, os.IsNotExist(err), "The removed directory came back")

		entries, err := os.ReadDir(filepath.Dir(dir))
		require.NoError(t, err, "Failed to list the parent directory")
		assert.Equal(t, 0, len(entries), "The directory moved aside was left behind")
	})

	t.Run("No-ops when the directory does not exist", func(t *testing.T) {
		dir := filepath.Join(t.TempDir(), "datadog-httpd")

		restore, remove, err := moveDirAside(dir)
		require.NoError(t, err, "Moving a missing directory aside failed")

		require.NoError(t, restore(), "Restoring a missing directory failed")
		require.NoError(t, remove(), "Removing a missing directory failed")

		_, err = os.Stat(dir)
		assert.Assert(t, os.IsNotExist(err), "A directory was created for one that never existed")
	})
}
