// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"

	log "github.com/sirupsen/logrus"
)

const InstallPath = "/opt/datadog-httpd"

type HttpdConfigurator struct {
	Version    string
	ConfigPath string
	Command    string
}

func (h *HttpdConfigurator) VerifyRequirements() error {

	log.Debug("Verifying httpd requirements")

	IntegrationName = "httpd"

	commands := []string{"httpd", "apachectl", "apache2", "apache2ctl"}
	found := false

	for _, cmd := range commands {
		if commandExists(cmd) {
			log.Debug(fmt.Sprintf("%s command found", cmd))
			h.Command = cmd
			found = true
			break
		}
	}

	if !found {
		return NewInstallerError(
			HttpdError,
			fmt.Errorf(
				"unsuccessfully attempted to find the httpd front end binary, "+
					"tried %s. Please ensure one of those binaries is included in the $PATH",
				strings.Join(commands, ", "),
			),
		)
	}

	log.Debugf("Running '%s -V'", h.Command)
	cmd := exec.Command(h.Command, "-V")
	output, err := cmd.CombinedOutput()
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("error running command '%s -V': %s", h.Command, string(output)))
	}

	log.Debugf("Successfully run '%s -V': %s", h.Command, string(output))

	versionRegex := regexp.MustCompile(`Server version: Apache/(\d+\.\d+\.\d+)`)
	matches := versionRegex.FindStringSubmatch(string(output))
	if len(matches) < 2 {
		return NewInstallerError(HttpdError, fmt.Errorf("failed to parse httpd version: %s", output))
	}

	h.Version = matches[1]

	log.Info("Detected httpd version: ", h.Version)
	IntegrationVersion = h.Version

	minSupportedVersion := "2.4.0"

	log.Debug("Minimum supported httpd version: ", minSupportedVersion)

	if compareVersions(h.Version, minSupportedVersion) < 0 {
		return NewInstallerError(HttpdError, fmt.Errorf("httpd version %s is not supported, must be higher than %s", h.Version, minSupportedVersion))
	}

	log.Info("The installed httpd version is supported")

	return nil
}

func (h *HttpdConfigurator) ModifyConfig(remoteConfigID, appID, site, clientToken, agentUri string, sessionSampleRate, sessionReplaySampleRate int, dryRun bool) error {

	log.Debug("Modifying httpd configuration")

	configPath, err := h.getDefaultConfigLocation()
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("failed to get config location: %v", err))
	}

	h.ConfigPath = configPath
	log.Debug("Found config file at: ", h.ConfigPath)

	customConfPath := fmt.Sprintf("%s/datadog.conf", InstallPath)
	log.Info("Creating .conf file with RUM configuration in ", customConfPath)
	content := datadogConfigFile(remoteConfigID, agentUri, appID, clientToken, site, sessionSampleRate, sessionReplaySampleRate)
	if err := os.WriteFile(customConfPath, []byte(content), 0644); err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("failed to write RUM configuration to '%s': %v", customConfPath, err))
	}

	if err := h.transformConfigHttpd(configPath, dryRun); err != nil {
		return err
	}

	return nil
}

func (h *HttpdConfigurator) ValidateConfig() error {

	if err := h.runConfigTest(h.ConfigPath); err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("config test failed: %w", err))
	}

	return nil
}

func (h *HttpdConfigurator) DownloadAndInstallModule(arch string, skipVerify bool) error {
	if arch == "arm64" {
		return NewInstallerError(HttpdError, fmt.Errorf("architecture '%s' is not supported for httpd", arch))
	}

	if err := h.createInstallDir(); err != nil {
		return err
	}

	moduleURL := fmt.Sprintf("https://rum-auto-instrumentation.s3.us-east-1.amazonaws.com/httpd/latest/mod_datadog-%s.zip", arch)

	moduleContent, err := downloadFile(moduleURL)
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("failed to download module: %v", err))
	}

	log.Debug("Creating temp directory")

	tmpDir, err := os.MkdirTemp("", "httpd-module-verification")
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("failed to create temp directory: %v", err))
	}

	log.Debug("Created temp directory: ", tmpDir)

	defer os.RemoveAll(tmpDir)

	moduleFile := filepath.Join(tmpDir, "httpd-rum.zip")
	signatureFile := filepath.Join(tmpDir, "httpd-rum.zip.asc")
	pubKeyFile := filepath.Join(tmpDir, "pubkey.gpg")

	if err := os.WriteFile(moduleFile, moduleContent, 0644); err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("failed to write module file: %v", err))
	}

	log.Debug("Downloaded and wrote module file: ", moduleURL)

	if !skipVerify {
		signatureURL := moduleURL + ".asc"
		signatureContent, err := downloadFile(signatureURL)
		if err != nil {
			return NewInstallerError(HttpdError, fmt.Errorf("failed to download signature: %v", err))
		}

		log.Debug("Downloaded signature file: ", signatureURL)

		if err := os.WriteFile(signatureFile, signatureContent, 0644); err != nil {
			return NewInstallerError(HttpdError, fmt.Errorf("failed to write signature file: %v", err))
		}

		log.Debug("Wrote signature file: ", signatureURL)

		pubKeyContent, err := downloadFile("https://keyserver.ubuntu.com/pks/lookup?op=get&search=0x2ed57717a223b2edf3281c427c74ee19de454d68")
		if err != nil {
			return NewInstallerError(HttpdError, fmt.Errorf("failed to download public key file: %v", err))
		}

		log.Debug("Downloaded public key file")

		if err := os.WriteFile(pubKeyFile, pubKeyContent, 0644); err != nil {
			return NewInstallerError(HttpdError, fmt.Errorf("failed to write public key file: %v", err))
		}

		if err := verifySignatureGPG(moduleFile, signatureFile, pubKeyFile); err != nil {
			return NewInstallerError(HttpdError, fmt.Errorf("failed to verify module signature. Bypass verification with --skipVerify: %v", err))
		}

		log.Debug("Verified module signature")
	}

	if err := unzipFile(moduleFile, InstallPath); err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("failed to unzip file: %v", err))
	}

	log.Infof("Module successfully downloaded, possibly verified, and installed in '%s'", InstallPath)
	return nil
}

func (n *HttpdConfigurator) GetFinalConfigMessage() string {
	return "Please gracefully restart httpd (or hard restart) for the changes to take effect"
}

// Drops the Include of the generated datadog.conf from the main config and
// removes the installation directory holding the module and that config.
func (h *HttpdConfigurator) Uninstall(dryRun bool) error {
	log.Debug("Uninstalling httpd module")

	configPath, err := h.getDefaultConfigLocation()
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("failed to get config location: %v", err))
	}

	h.ConfigPath = configPath
	log.Debug("Found config file at: ", configPath)

	content, err := os.ReadFile(configPath)
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("failed to read config file: %v", err))
	}

	newContent, err := removeDatadogFromHttpdConfig(content)
	if err != nil {
		return err
	}

	candidatePath, err := writeConfigCandidate(configPath, newContent)
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("failed to create intermediate config file: %v", err))
	}

	log.Info(fmt.Sprintf("Modified httpd configuration based on '%s', into intermediate config file '%s'", configPath, candidatePath))

	// Kept only for a dry run, where the whole point is to leave it for inspection.
	if !dryRun {
		defer os.Remove(candidatePath)
	}

	// Everything that can fail is done against the candidate first, so the common
	// failure leaves the live config exactly as it was.
	if err := h.runConfigTest(candidatePath); err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("intermediate config '%s' is invalid: %v", candidatePath, err))
	}

	if dryRun {
		log.Info("Dry run enabled, skipping config file modifications. Also skipping the check that nothing else references '" +
			InstallPath + "', since it briefly moves that directory away from a config still pointing at it. " +
			"The config to be used in a non dry run is: " + candidatePath)
		return nil
	}

	// httpd has no way to dump its expanded configuration on every version we
	// support ('-D DUMP_INCLUDES' needs 2.4.34, we support 2.4.0), so the check is
	// to make the directory unavailable and see whether the config still passes.
	// A surviving reference — a duplicate Include, or one in a file we never parse
	// — turns the test into a failure, instead of passing against a file that is
	// about to be deleted. The directory stays aside until the end, so the live
	// config is validated in the state it will actually run in.
	restoreInstallDir, removeInstallDir, err := moveDirAside(InstallPath)
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("failed to check whether '%s' is still referenced: %v", InstallPath, err))
	}

	defer func() {
		if err := restoreInstallDir(); err != nil {
			log.Error("Failed to put the httpd installation directory back: ", err)
		}
	}()

	if err := h.runConfigTest(candidatePath); err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("with '%s' unavailable the config '%s' no longer validates, so something still references it and removing it "+
			"would leave httpd unable to start. Please remove the remaining references and run the uninstall again: %v", InstallPath, candidatePath, err))
	}

	log.Debug("Nothing else references ", InstallPath)

	backupFile, err := replaceConfigFile(configPath, ".uninstall.backup", newContent)
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("failed to update config '%s': %v", configPath, err))
	}

	log.Info(fmt.Sprintf("Updated config '%s', with the original backed up as '%s'", configPath, backupFile))

	if err := h.runConfigTest(configPath); err != nil {
		if restoreErr := restoreConfigFile(configPath, backupFile); restoreErr != nil {
			return NewInstallerError(HttpdError, fmt.Errorf("config '%s' is invalid after removing the datadog Include (%v), and restoring it from '%s' failed too: %v",
				configPath, err, backupFile, restoreErr))
		}

		return NewInstallerError(HttpdError, fmt.Errorf("config '%s' is invalid after removing the datadog Include, so the original was restored: %v", configPath, err))
	}

	log.Info("Removing httpd installation directory: ", InstallPath)

	if err := removeInstallDir(); err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("failed to remove httpd installation directory '%s': %v", InstallPath, err))
	}

	log.Info("Successfully removed httpd installation directory: ", InstallPath)

	return nil
}

// Inverse of the Include line appended by transformConfigHttpd. The blank line
// separating it from the original content goes too, so the config round-trips.
//
// Every occurrence goes, not just the first. A duplicate Include left behind
// still names a file that is about to be deleted along with the installation
// directory, and it passes the config test while the file is still there — so
// Apache only refuses to start later, once the directory is gone.
func removeDatadogFromHttpdConfig(content []byte) ([]byte, error) {
	contentStr := string(content)

	// [ \t] rather than [\s], so the match cannot reach back across a newline and
	// take a line that belongs to the original config with it.
	includeRegex := regexp.MustCompile(`(?m)^[ \t]*Include[ \t]+` + regexp.QuoteMeta(InstallPath) + `/datadog\.conf[ \t]*(\n|$)`)
	matches := includeRegex.FindAllStringIndex(contentStr, -1)
	if len(matches) == 0 {
		return nil, NewInstallerError(HttpdError, fmt.Errorf("the datadog module does not appear to be installed: no 'Include %s/datadog.conf' line found in the httpd config", InstallPath))
	}

	log.Debug("Removing ", len(matches), " datadog Include line(s) from the httpd config")

	// Spliced back to front so the offsets of the earlier matches stay valid.
	for i := len(matches) - 1; i >= 0; i-- {
		// transformConfigHttpd writes the Include after a blank line, so that
		// separator goes too whenever it is there.
		start := matches[i][0]
		if start >= 2 && contentStr[start-1] == '\n' && contentStr[start-2] == '\n' {
			start--
		}

		contentStr = contentStr[:start] + contentStr[matches[i][1]:]
	}

	return []byte(contentStr), nil
}

func (h *HttpdConfigurator) getDefaultConfigLocation() (string, error) {
	log.Debugf("Getting config location running '%s -V'", h.Command)

	cmd := exec.Command(h.Command, "-V")
	output, err := cmd.CombinedOutput()
	if err != nil {
		return "", NewInstallerError(HttpdError, fmt.Errorf("failed to execute '%s -V': %v. Output:\n%s", h.Command, err, output))
	}

	pattern := regexp.MustCompile(`SERVER_CONFIG_FILE="([^"]+)"`)
	matches := pattern.FindStringSubmatch(string(output))

	if len(matches) < 2 {
		return "", NewInstallerError(HttpdError, fmt.Errorf("failed to find SERVER_CONFIG_FILE in: %s", output))
	}

	configPath := matches[1]

	// Absolute path
	if strings.HasPrefix(matches[1], "/") {
		return configPath, nil
	}

	log.Debugf("Detected relative path for config '%s', looking for root path", configPath)

	pattern = regexp.MustCompile(`HTTPD_ROOT="([^"]+)"`)
	matches = pattern.FindStringSubmatch(string(output))

	if len(matches) < 2 {
		return "", NewInstallerError(HttpdError, fmt.Errorf("failed to find HTTPD_ROOT in: %s", output))
	}

	log.Debug("Found root path: ", matches[1])

	return filepath.Join(matches[1], configPath), nil
}

func (h *HttpdConfigurator) createInstallDir() error {
	if err := os.MkdirAll(InstallPath, 0644); err == nil {
		log.Debug("Created installation directory: ", InstallPath)

		return nil
	}

	return NewInstallerError(HttpdError, fmt.Errorf("could not create httpd installation directory: %s", InstallPath))
}

func (h *HttpdConfigurator) transformConfigHttpd(src string, dryRun bool) error {
	configLine := fmt.Sprintf("Include %s/datadog.conf", InstallPath)

	err := h.runConfigTest(src)
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("error running config test before modifications to the system, can't continue until fixed: %w", err))
	}

	content, err := os.ReadFile(src)
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("error reading file: %w", err))
	}

	if strings.Contains(string(content), configLine) {
		return NewInstallerError(HttpdError, fmt.Errorf("datadog module is already installed, can't perform installation"))
	}

	newContent := []byte(fmt.Sprintf("%s\n%s\n", string(content), configLine))

	candidatePath, err := writeConfigCandidate(src, newContent)
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("error writing new configuration file: %w", err))
	}

	log.Debug("Wrote the modified config file in ", candidatePath)

	// Kept only for a dry run, where the whole point is to leave it for inspection.
	if !dryRun {
		defer os.Remove(candidatePath)
	}

	if err := h.runConfigTest(candidatePath); err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("error running config test after modifications to the system: %w", err))
	}

	if dryRun {
		log.Info("Dry run enabled, skipping config file modifications. The config to be used in a non dry run is: ", candidatePath)
		return nil
	}

	backupFile, err := replaceConfigFile(src, ".bak", newContent)
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("error updating configuration file '%s': %w", src, err))
	}

	log.Info("Configuration updated successfully, with the original backed up as ", backupFile)

	return nil
}

func (h *HttpdConfigurator) runConfigTest(configFile string) error {
	log.Debugf("Testing configuration: %s -t -f %s", h.Command, configFile)
	cmd := exec.Command(h.Command, "-t", "-f", configFile)
	output, err := cmd.CombinedOutput()
	if err != nil {
		return NewInstallerError(HttpdError, fmt.Errorf("config test failed: %w. Output: %s", err, output))
	}
	log.Info("Config validated successfully: ", configFile)

	return nil
}

func datadogConfigFile(remoteConfigID, agentUri, appID, clientToken, site string, sessionSampleRate, sessionReplaySampleRate int) string {
	content := fmt.Sprintf(`LoadModule datadog_module %s/mod_datadog.so
	DatadogTracing Off
	DatadogRum On
	DatadogAgentUrl "%s"
	<DatadogRumSettings "%s">
		DatadogRumOption "applicationId" "%s"
		DatadogRumOption "clientToken" "%s"
		DatadogRumOption "site" "%s"`, InstallPath, agentUri, "v6", appID, clientToken, site)

	if remoteConfigID != "" {
		content += fmt.Sprintf(`
		DatadogRumOption "remoteConfigurationId" "%s"`, remoteConfigID)
	} else {
		content += fmt.Sprintf(`
		DatadogRumOption "sessionSampleRate" "%d"
		DatadogRumOption "sessionReplaySampleRate" "%d"`, sessionSampleRate, sessionReplaySampleRate)
	}

	content += `
	</DatadogRumSettings>
	`

	return content
}
