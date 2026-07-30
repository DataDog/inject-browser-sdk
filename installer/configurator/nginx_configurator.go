// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"strings"

	log "github.com/sirupsen/logrus"
)

const NginxModulesPath = "/opt/datadog-nginx"

type NginxConfigurator struct {
	Version     string
	ModulesPath string
}

// Runs 'nginx -v' and looks for the nginx version.
// Checks that the version is higher than a min version
func (n *NginxConfigurator) VerifyRequirements() error {

	log.Debug("Verifying NGINX requirements")

	IntegrationName = "nginx"

	cmd := exec.Command("nginx", "-v")
	output, err := cmd.CombinedOutput()
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("'nginx' command was not detected. Please ensure NGINX is included in the PATH"))
	}

	log.Debug("Successfully run 'nginx -v': ", string(output))

	versionRegex := regexp.MustCompile(`nginx version: nginx/(\d+\.\d+\.\d+)`)
	matches := versionRegex.FindStringSubmatch(string(output))
	if len(matches) < 2 {
		return NewInstallerError(NginxError, fmt.Errorf("failed to parse NGINX version: %s", output))
	}

	n.Version = matches[1]

	log.Info("Detected NGINX version: ", n.Version)
	IntegrationVersion = n.Version

	return nil
}

// Reads the config file
// 1. Checks if a line containing the module already exists
// 2. Creates a backup of the config file
// 3. Appends the load_module line to the beginning of the file
// 4. Inserts the RUM config as early as possible in the http section
func (n *NginxConfigurator) ModifyConfig(remoteConfigID, appID, site, clientToken, agentUri string, sessionSampleRate, sessionReplaySampleRate int, dryRun bool) error {

	log.Debug("Modifying NGINX configuration")

	configPath, err := n.getDefaultConfigLocation()
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed to get config location: %v", err))
	}

	log.Debug("Found config file at: ", configPath)

	content, err := os.ReadFile(configPath)
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed to read config file: %v", err))
	}

	log.Debug("Successfully read config file")

	// Check if Datadog module is already loaded
	if err := n.testNginxLoadsModule(n.ModulesPath + "/ngx_http_datadog_module.so"); err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("datadog module could not be loaded, likely because the module is already in use: %v", err))
	}

	newContent, err := addDatadogToConfig(n, content, remoteConfigID, agentUri, appID, clientToken, site, sessionSampleRate, sessionReplaySampleRate)
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed modifying config config: %v", err))
	}

	candidatePath, err := writeConfigCandidate(configPath, newContent)
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed to create intermediate config file: %v", err))
	}

	log.Info(fmt.Sprintf("Modified NGINX configuration based on '%s', into intermediate config file '%s'", configPath, candidatePath))

	// Kept only for a dry run, where the whole point is to leave it for inspection.
	if !dryRun {
		defer os.Remove(candidatePath)
	}

	if err := n.validateConfig(candidatePath); err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("intermediate config '%s' is invalid: %v", candidatePath, err))
	}

	log.Debug("Intermediate config is valid: ", candidatePath)

	if dryRun {
		log.Info("Dry run enabled, skipping config file modifications. The config to be used in a non dry run is: ", candidatePath)
		return nil
	}

	backupPath, err := replaceConfigFile(configPath, ".backup", newContent)
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed to update config '%s': %v", configPath, err))
	}

	log.Info(fmt.Sprintf("Updated config '%s', with the original backed up as '%s'", configPath, backupPath))

	return nil
}

// Runs 'nginx -t' to validate the configuration
func (n *NginxConfigurator) ValidateConfig() error {
	return n.validateConfig("")
}

// Downloads the latest nginx-datadog for the specified architecture
// Extracts the .so file and places it in the modules path
func (n *NginxConfigurator) DownloadAndInstallModule(arch string, skipVerify bool) error {
	if err := n.createModulesDir(); err != nil {
		return err
	}

	baseURL := "https://rum-auto-instrumentation.s3.us-east-1.amazonaws.com/nginx/latest/"
	moduleURL := baseURL + fmt.Sprintf("ngx_http_datadog_module-%s-%s.so.tgz", arch, n.Version)

	moduleContent, err := downloadFile(moduleURL)
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed to download module %s. "+
			"This is likely because this NGINX version (%s) is not supported: %v", moduleURL, n.Version, err))
	}

	tmpDir, err := os.MkdirTemp("", "nginx-module-verification")
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed to create temp directory: %v", err))
	}

	log.Debug("Created temp directory: ", tmpDir)

	defer os.RemoveAll(tmpDir)

	moduleFile := filepath.Join(tmpDir, "module.tgz")
	signatureFile := filepath.Join(tmpDir, "module.tgz.asc")
	publicKeyFile := filepath.Join(tmpDir, "pubkey.gpg")

	if err := os.WriteFile(moduleFile, moduleContent, 0644); err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed to write module file: %v", err))
	}

	log.Debug("Downloaded module file: ", moduleURL)

	if !skipVerify {
		signatureURL := moduleURL + ".asc"
		signatureContent, err := downloadFile(signatureURL)
		if err != nil {
			return NewInstallerError(NginxError, fmt.Errorf("failed to download signature: %v", err))
		}

		log.Debug("Downloaded signature file: ", signatureURL)

		publicKeyURL := baseURL + "pubkey.gpg"
		publicKeyContent, err := downloadFile(publicKeyURL)
		if err != nil {
			return NewInstallerError(NginxError, fmt.Errorf("failed to download public key: %v", err))
		}

		log.Debug("Downloaded public key file: ", publicKeyURL)

		if err := os.WriteFile(signatureFile, signatureContent, 0644); err != nil {
			return NewInstallerError(NginxError, fmt.Errorf("failed to write signature file: %v", err))
		}

		log.Debug("Wrote signature file: ", signatureURL)

		if err := os.WriteFile(publicKeyFile, publicKeyContent, 0644); err != nil {
			return NewInstallerError(NginxError, fmt.Errorf("failed to write public key file: %v", err))

		}
		log.Debug("Wrote public key file: ", publicKeyURL)

		if err := verifySignatureGPG(moduleFile, signatureFile, publicKeyFile); err != nil {
			return NewInstallerError(NginxError, fmt.Errorf("failed to verify module signature. Bypass verification with --skipVerify: %v", err))
		}

		log.Debug("Verified module signature")
	}

	if err := extractTarGzFile(moduleFile, n.ModulesPath); err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed to extract tgz file: %v", err))
	}

	log.Info(fmt.Sprintf("Module successfully downloaded, possibly verified, and installed in %s", n.ModulesPath))
	return nil
}

func (n *NginxConfigurator) validateConfig(configPath string) error {
	var cmd *exec.Cmd

	if configPath != "" {
		cmd = exec.Command("nginx", "-c", configPath, "-t")
	} else {
		cmd = exec.Command("nginx", "-t")
	}

	configInfo := "default location"
	if configPath != "" {
		configInfo = configPath
	}

	output, err := cmd.CombinedOutput()
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("NGINX configuration validation failed for config at '%s' with output: %s. %v", configInfo, output, err))
	}

	log.Debug("Successfully validated nginx configuration at ", configInfo)

	return nil
}

// Runs 'nginx -T' to get the fully expanded configuration: every include
// resolved, each source file introduced by a '# configuration file <path>:' line.
// This is the only way to see directives living in includes we never parse
// ourselves, such as conf.d/*.conf.
func (n *NginxConfigurator) dumpConfig(configPath string) (string, error) {
	cmd := exec.Command("nginx", "-T", "-c", configPath)
	output, err := cmd.CombinedOutput()
	if err != nil {
		return "", NewInstallerError(NginxError, fmt.Errorf("failed to dump the expanded nginx configuration for '%s': %v. Output:\n%s", configPath, err, output))
	}

	return string(output), nil
}

// Refuses to delete the modules directory while the configuration still points
// at it. Deleting it anyway would pass every config test we run — the module file
// is still there while we test — and leave NGINX unable to start afterwards.
func (n *NginxConfigurator) verifyModulesPathUnreferenced(configPath string) error {
	dump, err := n.dumpConfig(configPath)
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("could not check whether '%s' is still referenced: %v", NginxModulesPath, err))
	}

	if source := findReferenceInNginxDump(dump, NginxModulesPath); source != "" {
		return NewInstallerError(NginxError, fmt.Errorf("'%s' is still referenced by '%s', so removing it would leave NGINX unable to start. "+
			"Please remove the remaining references and run the uninstall again", NginxModulesPath, source))
	}

	log.Debug("Nothing in the expanded configuration references ", NginxModulesPath)

	return nil
}

// Scans an 'nginx -T' dump for a reference to needle and returns the config file
// it was found in, or "" when there is none. The '# configuration file <path>:'
// annotations are what make the file attributable; they are skipped as content,
// since one of them naming a file under needle is not itself a reference.
func findReferenceInNginxDump(dump, needle string) string {
	const marker = "# configuration file "

	source := "the expanded configuration"
	for _, line := range strings.Split(dump, "\n") {
		if trimmed := strings.TrimSpace(line); strings.HasPrefix(trimmed, marker) {
			source = strings.TrimSuffix(strings.TrimSpace(strings.TrimPrefix(trimmed, marker)), ":")
			continue
		}

		if strings.Contains(line, needle) {
			return source
		}
	}

	return ""
}

func (n *NginxConfigurator) GetFinalConfigMessage() string {
	return "Please reload NGINX or restart the service for the changes to take effect"
}

// Modifies the configuration file to exclude previously added datadog_ directives
// Removes the installation directory
func (n *NginxConfigurator) Uninstall(dryRun bool) error {
	log.Debug("Uninstalling NGINX module")

	configPath, err := n.getDefaultConfigLocation()
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed to get config location: %v", err))
	}

	log.Debug("Found config file at: ", configPath)

	content, err := os.ReadFile(configPath)
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed to read config file: %v", err))
	}

	log.Debug("Successfully read config file")

	newContent, err := removeDatadogFromConfig(content)
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed modifying config: %v", err))
	}

	candidatePath, err := writeConfigCandidate(configPath, newContent)
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed to create intermediate config file: %v", err))
	}

	log.Info(fmt.Sprintf("Modified NGINX configuration based on '%s', into intermediate config file '%s'", configPath, candidatePath))

	if !dryRun {
		defer os.Remove(candidatePath)
	}

	// Everything that can fail is done against the candidate first, so the common
	// failure leaves the live config exactly as it was.
	if err := n.validateConfig(candidatePath); err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("intermediate config '%s' is invalid: %v", candidatePath, err))
	}

	log.Debug("Intermediate config is valid: ", candidatePath)

	if err := n.verifyModulesPathUnreferenced(candidatePath); err != nil {
		return err
	}

	if dryRun {
		log.Info("Dry run enabled, exiting before modifying the installation. The config to be used in a non dry run is: ", candidatePath)
		return nil
	}

	backupPath, err := replaceConfigFile(configPath, ".uninstall.backup", newContent)
	if err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed to update config '%s': %v", configPath, err))
	}

	log.Info(fmt.Sprintf("Updated config '%s', with the original backed up as '%s'", configPath, backupPath))

	if err := n.ValidateConfig(); err != nil {
		if restoreErr := restoreConfigFile(configPath, backupPath); restoreErr != nil {
			return NewInstallerError(NginxError, fmt.Errorf("config '%s' is invalid after removing the datadog directives (%v), and restoring it from '%s' failed too: %v",
				configPath, err, backupPath, restoreErr))
		}

		return NewInstallerError(NginxError, fmt.Errorf("config '%s' is invalid after removing the datadog directives, so the original was restored: %v", configPath, err))
	}

	log.Info("Removing nginx modules directory: " + NginxModulesPath)

	if err := os.RemoveAll(NginxModulesPath); err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("failed to remove nginx modules directory '%s': %v", NginxModulesPath, err))
	}

	log.Info("Successfully removed nginx modules directory: ", NginxModulesPath)

	return nil
}

// Insert load_module directive at the beginning of the file
// Find http and bracket { on possibly multiple lines
// Insert the datadog configuration after the http block
// Return the modified content
func addDatadogToConfig(n *NginxConfigurator, content []byte, remoteConfigID, agentUri string, appID string, clientToken string, site string, sessionSampleRate int, sessionReplaySampleRate int) ([]byte, error) {
	var newContent strings.Builder
	newContent.WriteString(fmt.Sprintf("load_module %s/ngx_http_datadog_module.so;\n", n.ModulesPath))

	httpRegex := regexp.MustCompile(`(?m)^[\s]*http[\s]*\{`)

	httpBlockStart := httpRegex.FindIndex(content)
	if httpBlockStart == nil {
		return nil, NewInstallerError(NginxError, fmt.Errorf("could not find http block in nginx config"))
	}

	newContent.Write(content[:httpBlockStart[0]])

	newContent.Write(content[httpBlockStart[0]:httpBlockStart[1]])
	newContent.WriteString("\n")

	datadogConfig := fmt.Sprintf(`    datadog_agent_url %s;

    # Disable APM Tracing. Remove the next line to enable APM Tracing.
    datadog_tracing off;

    # Enable RUM Injection
    datadog_rum on;

    datadog_rum_config "v6" {
        "applicationId" "%s";
        "clientToken" "%s";
        "site" "%s";`, agentUri, appID, clientToken, site)
	if remoteConfigID != "" {
		datadogConfig += fmt.Sprintf(`
        "remoteConfigurationId" "%s";`, remoteConfigID)
	} else {
		datadogConfig += fmt.Sprintf(`
        "sessionSampleRate" "%d";
        "sessionReplaySampleRate" "%d";`, sessionSampleRate, sessionReplaySampleRate)
	}

	datadogConfig += `
    }
`

	newContent.WriteString(datadogConfig)

	newContent.Write(content[httpBlockStart[1]:])

	return []byte(newContent.String()), nil
}

// Simple datadog_* directives written by addDatadogToConfig, keyed by directive
// name. datadog_rum_config is handled separately: it opens a block.
var nginxDatadogDirectives = []string{"datadog_agent_url", "datadog_tracing", "datadog_rum"}

const nginxDatadogBlockDirective = "datadog_rum_config"

// Distinctive fragments of the comments addDatadogToConfig writes. Matched as
// substrings so both the current wording and the older "Remove this line" variant
// are covered, and so a comment is recognised on its own text rather than only
// through the directive underneath it — which may well have been deleted by hand,
// since one of these comments invites exactly that.
var nginxInstallerComments = []string{"Disable APM Tracing", "Enable RUM Injection"}

// Inverse of addDatadogToConfig: drops every datadog load_module directive and
// every datadog_* directive it wrote inside the http block, restoring the
// original config byte for byte.
//
// The comment lines above datadog_tracing and datadog_rum are dropped along with
// the directive they document, so a config written by any installer version
// round-trips whether the comments sit on their own line or trail the directive.
//
// Only load_module has to be there. The directives inside the http block are all
// optional on removal: the installer's own comment tells users to delete
// datadog_tracing to enable APM tracing, and datadog_agent_url only restates the
// module's default. Demanding them would turn following our own documentation
// into an uninstall that refuses to run. The candidate config is handed to
// 'nginx -t' before anything on disk is touched, and that is what catches a
// directive left dangling in an include we never parse.
func removeDatadogFromConfig(content []byte) ([]byte, error) {
	contentStr := string(content)

	// Checked up front so a config we never wrote is refused before anything else
	// is reported about it, but removed further down against the assembled string.
	//
	// [ \t] rather than [\s] around the directive, and a single optional newline at
	// the end: [\s]* would reach across newlines and take blank lines belonging to
	// the original config with it. The nginx docker images ship a config whose very
	// first byte is a newline, and the load_module line is written directly above
	// it, so that blank line is immediately after the match.
	moduleRegex := regexp.MustCompile(`(?m)^[ \t]*load_module[ \t]+.*/ngx_http_datadog_module\.so;[ \t]*\r?\n?`)
	modules := moduleRegex.FindAllStringIndex(contentStr, -1)
	if len(modules) == 0 {
		return nil, NewInstallerError(NginxError, fmt.Errorf("datadog load_module directive not found in nginx config"))
	}

	httpRegex := regexp.MustCompile(`(?m)^[\s]*http[\s]*\{`)
	httpBlockStart := httpRegex.FindStringIndex(contentStr)
	if httpBlockStart == nil {
		return nil, NewInstallerError(NginxError, fmt.Errorf("could not find http block in nginx config"))
	}

	httpContent := contentStr[httpBlockStart[1]:]
	httpContentEnd := findBlockEnd(httpContent)
	if httpContentEnd < 0 {
		return nil, NewInstallerError(NginxError, fmt.Errorf("malformed http block in nginx config"))
	}

	httpBlock := httpContent[:httpContentEnd]
	httpBlockWithoutDatadog, err := removeDatadogDirectives(httpBlock)
	if err != nil {
		return nil, err
	}

	if httpBlockWithoutDatadog == httpBlock {
		log.Warn("No datadog_* directives were found in the http block of the nginx config; only the load_module directive will be removed")
	}

	// The load_module directives are matched on the assembled string rather than
	// on the original, so their offsets can never be spliced against the
	// separately computed http block offsets — a load_module sitting after
	// 'http {' used to make those two index pairs cross.
	assembled := contentStr[:httpBlockStart[1]] + httpBlockWithoutDatadog + httpContent[httpContentEnd:]

	// Every occurrence goes, not just the first: leaving a duplicate behind would
	// keep the config pointing at a module directory that is about to be deleted.
	log.Debug("Removing ", len(modules), " datadog load_module directive(s)")

	return []byte(moduleRegex.ReplaceAllString(assembled, "")), nil
}

// Walks the lines of an http block and drops the datadog_* directives, the
// comments documenting them, and the blank line each one was written with.
func removeDatadogDirectives(httpBlock string) (string, error) {
	lines := strings.Split(httpBlock, "\n")

	var kept []string
	// addDatadogToConfig writes an empty line before each directive it adds, so
	// dropping that separator alongside the directive is what makes the removal
	// an exact inverse. Whitespace-only lines are left alone: they belong to the
	// original config, not to the block the installer wrote.
	dropSeparator := func() {
		if len(kept) > 0 && kept[len(kept)-1] == "" {
			kept = kept[:len(kept)-1]
		}
	}

	for i := 0; i < len(lines); i++ {
		directive := nginxDirectiveName(lines[i])

		// A comment documents the directive below it, so it goes when that does —
		// or when the comment is one we recognise as ours, since the directive it
		// documented may have been deleted by hand. Without the second test the
		// comment would survive and accrete on every install/uninstall cycle.
		if strings.HasPrefix(strings.TrimSpace(lines[i]), "#") {
			if nextDatadogDirective(lines[i+1:]) != "" || isNginxInstallerComment(lines[i]) {
				dropSeparator()
				continue
			}
		}

		if directive == nginxDatadogBlockDirective {
			block := strings.Join(lines[i:], "\n")
			openingBrace := strings.IndexByte(block, '{')
			if openingBrace < 0 {
				return "", NewInstallerError(NginxError, fmt.Errorf("%s is not followed by a block in the nginx config", nginxDatadogBlockDirective))
			}

			blockEnd := findBlockEnd(block[openingBrace+1:])
			if blockEnd < 0 {
				return "", NewInstallerError(NginxError, fmt.Errorf("malformed %s block in nginx config", nginxDatadogBlockDirective))
			}

			dropSeparator()
			// Skip past the line holding the closing brace.
			i += strings.Count(block[:openingBrace+1+blockEnd], "\n")
			continue
		}

		if slices.Contains(nginxDatadogDirectives, directive) {
			dropSeparator()
			continue
		}

		kept = append(kept, lines[i])
	}

	return strings.Join(kept, "\n"), nil
}

// Reports whether a comment line is one addDatadogToConfig wrote.
func isNginxInstallerComment(line string) bool {
	for _, comment := range nginxInstallerComments {
		if strings.Contains(line, comment) {
			return true
		}
	}

	return false
}

// Returns the directive name a config line opens with, without its trailing
// semicolon: "    datadog_rum on;" yields "datadog_rum".
func nginxDirectiveName(line string) string {
	fields := strings.Fields(line)
	if len(fields) == 0 {
		return ""
	}

	return strings.TrimSuffix(fields[0], ";")
}

// Returns the datadog_* directive opening the next non-blank line, if any.
func nextDatadogDirective(lines []string) string {
	for _, line := range lines {
		if strings.TrimSpace(line) == "" {
			continue
		}

		directive := nginxDirectiveName(line)
		if directive == nginxDatadogBlockDirective || slices.Contains(nginxDatadogDirectives, directive) {
			return directive
		}

		return ""
	}

	return ""
}

// Returns the index of the brace closing a block that has already been opened,
// or -1 if the braces never balance. Counting braces rather than matching a
// regex keeps nested blocks intact.
func findBlockEnd(afterOpeningBrace string) int {
	depth := 1
	for i, r := range afterOpeningBrace {
		switch r {
		case '{':
			depth++
		case '}':
			depth--
			if depth == 0 {
				return i
			}
		}
	}

	return -1
}

func (n *NginxConfigurator) testNginxLoadsModule(modulePath string) error {
	// Ensure test without changes succeeds
	if err := n.validateConfig(""); err != nil {
		return NewInstallerError(NginxError, fmt.Errorf("NGINX configuration test failed with the current config: %s", err))
	}

	log.Debug("NGINX default configuration test succeeded before modifications")

	// Test again loading the module
	cmd := exec.Command("nginx", "-g", fmt.Sprintf("load_module %s;", modulePath), "-t")
	output, err := cmd.CombinedOutput()
	if err != nil {
		if strings.Contains(string(output), "already loaded") {
			return NewInstallerError(NginxError, fmt.Errorf("datadog module is already loaded, can't perform installation: %s\n%s", err, string(output)))
		}
		return NewInstallerError(NginxError, fmt.Errorf("NGINX configuration test with module failed with unexpected error: %s\n%s", err, string(output)))
	}

	log.Debug("Successfully validated default configuration with module loaded")

	return nil
}

// Gets the configuration file from 'nginx -T'
func (n *NginxConfigurator) getDefaultConfigLocation() (string, error) {
	log.Debug("Getting default config location running 'nginx -T'")

	cmd := exec.Command("nginx", "-T")
	output, err := cmd.CombinedOutput()
	if err != nil {
		return "", NewInstallerError(NginxError, fmt.Errorf("failed to execute nginx -T: %v. output:\n%s", err, output))
	}

	lines := strings.Split(string(output), "\n")
	for _, line := range lines {
		if strings.HasPrefix(line, "# configuration file ") {
			configFile := strings.TrimPrefix(line, "# configuration file ")
			configFile = strings.TrimSpace(configFile)
			configFile = strings.TrimSuffix(configFile, ":")

			log.Debug("Found default config file: ", configFile)

			return strings.TrimSpace(configFile), nil
		}
	}

	return "", NewInstallerError(NginxError, fmt.Errorf("could not find configuration file in nginx -T output:\n%s", output))
}

func (n *NginxConfigurator) createModulesDir() error {
	if err := os.MkdirAll(NginxModulesPath, 0644); err == nil {
		n.ModulesPath = NginxModulesPath
		log.Debug("Created nginx modules directory: ", NginxModulesPath)

		return nil
	}

	return NewInstallerError(NginxError, fmt.Errorf("could not create nginx modules directory: %s", NginxModulesPath))
}
