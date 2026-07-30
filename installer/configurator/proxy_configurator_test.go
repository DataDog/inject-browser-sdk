// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	log "github.com/sirupsen/logrus"
	"github.com/stretchr/testify/require"
	"gotest.tools/assert"
)

type FileInfo struct {
	Filename string
	IsError  bool
}

func TestTransformNginxConfigSnapshot(t *testing.T) {
	const modulesPath = "/opt/datadog-nginx"
	const agentUri = "http://localhost:8126"
	const appId = "ffffffff-ffff-ffff-ffff-ffffffffffff"
	const clientToken = "pubffffffffffffffffffffffffffffffff"
	const site = "datadoghq.com"
	const sessionSampleRate = 49
	const sessionReplaySampleRate = 51
	const remoteConfigId = ""

	pattern := filepath.Join("testdata", "*.conf")
	files, err := filepath.Glob(pattern)
	require.NoError(t, err, "Failed to list test cases")

	// List all test cases
	// *.conf should contain a corresponding *.conf.snap file, created if not exists
	// *.err.conf should fail the transformation, no corresponding snapshot file
	var testCases []FileInfo
	for _, file := range files {
		isError := strings.HasSuffix(filepath.Base(file), ".err.conf")
		testCases = append(testCases, FileInfo{
			Filename: file,
			IsError:  isError,
		})
	}

	for _, tc := range testCases {
		t.Run(tc.Filename, func(t *testing.T) {
			input, err := os.ReadFile(tc.Filename)
			require.NoError(t, err, "Failed to read input file")

			configurator := &NginxConfigurator{
				ModulesPath: modulesPath,
			}

			transformed, err := addDatadogToConfig(configurator, input, remoteConfigId, agentUri, appId, clientToken, site, sessionSampleRate, sessionReplaySampleRate)
			if tc.IsError {
				require.Error(t, err, "Expected an error transforming the configuration, it succeeded instead")
				return
			}

			require.NoError(t, err, "Failed to transform config")

			snapshotFile := filepath.Join("testdata", filepath.Base(tc.Filename)+".snap")

			if _, err := os.Stat(snapshotFile); os.IsNotExist(err) {
				// If snapshot doesn't exist, create it for local use, fail the test
				err = os.WriteFile(snapshotFile, transformed, 0644)
				require.NoError(t, err, "Failed to create snapshot file")
				t.Logf("Created new snapshot: %s", snapshotFile)
				t.Fail()
			} else {
				// If snapshot exists, compare with it
				expected, err := os.ReadFile(snapshotFile)
				require.NoError(t, err, "Failed to read snapshot file")

				assert.Equal(t, string(expected), string(transformed), "Transformed config does not match snapshot")
			}
		})
		t.Run(tc.Filename+" (removal round-trip)", func(t *testing.T) {
			snapshotFile := filepath.Join("testdata", filepath.Base(tc.Filename)+".snap")
			if tc.IsError {
				snapshotFile = tc.Filename
			}

			// Read the snapshot file (with Datadog config)
			input, err := os.ReadFile(snapshotFile)
			require.NoError(t, err, "Failed to read snapshot file")

			// Apply removeDatadogFromConfig to get back the original config
			transformed, err := removeDatadogFromConfig(input)

			if tc.IsError {
				require.Error(t, err, "Expected an error removing the Datadog configuration, it succeeded instead")
				return
			}

			require.NoError(t, err, "Failed to remove Datadog from config")

			// Get the original file (without Datadog config) to compare against
			expected, err := os.ReadFile(tc.Filename)
			require.NoError(t, err, "Failed to read original file")

			// Compare the transformed config with the original file
			assert.Equal(t, string(expected), string(transformed), "Config after Datadog removal does not match original config")
		})
	}
}

// Installs into testdata/basic.conf and returns both the original config and the
// installed one, so removal tests start from realistic installer output.
func installIntoBasicConfig(t *testing.T) (original string, installed string) {
	t.Helper()

	content, err := os.ReadFile(filepath.Join("testdata", "basic.conf"))
	require.NoError(t, err, "Failed to read basic.conf")

	return string(content), installInto(t, string(content))
}

func installInto(t *testing.T, original string) string {
	t.Helper()

	configurator := &NginxConfigurator{ModulesPath: "/opt/datadog-nginx"}
	transformed, err := addDatadogToConfig(
		configurator, []byte(original), "", "http://localhost:8126",
		"ffffffff-ffff-ffff-ffff-ffffffffffff", "pubffffffffffffffffffffffffffffffff",
		"datadoghq.com", 49, 51,
	)
	require.NoError(t, err, "Failed to add Datadog to config")

	return string(transformed)
}

// The nginx docker images ship a config whose very first byte is a newline, and
// load_module is written directly above it. A removal that consumed trailing
// whitespace greedily took that blank line with it, so the config came back one
// byte short of the original.
func TestRemoveDatadogFromConfigWithLeadingBlankLine(t *testing.T) {
	content, err := os.ReadFile(filepath.Join("testdata", "basic.conf"))
	require.NoError(t, err, "Failed to read basic.conf")

	original := "\n" + string(content)

	transformed, err := removeDatadogFromConfig([]byte(installInto(t, original)))
	require.NoError(t, err, "Failed to remove Datadog from config")

	assert.Equal(t, original, string(transformed), "The config's leading blank line did not survive removal")
}

// A datadog_rum_config block holding a nested block must be removed whole. The
// generated config never nests today, so nesting is spliced into real installer
// output rather than hand-written, keeping the surrounding shape realistic.
func TestRemoveDatadogFromConfigWithNestedBlock(t *testing.T) {
	original, installed := installIntoBasicConfig(t)

	nested := `        "applicationId" "ffffffff-ffff-ffff-ffff-ffffffffffff";
        "nested" {
            "deep" "value";
        }`
	withNesting := strings.Replace(installed,
		`        "applicationId" "ffffffff-ffff-ffff-ffff-ffffffffffff";`, nested, 1)
	require.NotEqual(t, installed, withNesting, "Failed to splice a nested block into the config")

	transformed, err := removeDatadogFromConfig([]byte(withNesting))
	require.NoError(t, err, "Failed to remove Datadog from config")

	assert.Equal(t, original, string(transformed), "Nested block was not removed cleanly")
}

// Configs written by an installer that trailed the comments after the directives
// instead of putting them on their own line must still uninstall cleanly.
func TestRemoveDatadogFromConfigWithTrailingComments(t *testing.T) {
	original, installed := installIntoBasicConfig(t)

	trailing := strings.NewReplacer(
		"    # Disable APM Tracing. Remove the next line to enable APM Tracing.\n    datadog_tracing off;",
		"    datadog_tracing off; # Disable APM Tracing. Remove this line to enable APM Tracing.",
		"    # Enable RUM Injection\n    datadog_rum on;",
		"    datadog_rum on; # Enable RUM Injection",
	).Replace(installed)
	require.NotEqual(t, installed, trailing, "Failed to rewrite the comments into trailing form")

	transformed, err := removeDatadogFromConfig([]byte(trailing))
	require.NoError(t, err, "Failed to remove Datadog from config")

	assert.Equal(t, original, string(transformed), "Trailing-comment config did not round-trip")
}

// The exact lines addDatadogToConfig writes into the http block, so hand-edits
// can be expressed as deletions of real installer output.
const (
	nginxTracingComment   = "    # Disable APM Tracing. Remove the next line to enable APM Tracing.\n"
	nginxTracingDirective = "    datadog_tracing off;\n"
	nginxRumComment       = "    # Enable RUM Injection\n"
	nginxRumDirective     = "    datadog_rum on;\n"
	nginxAgentUrl         = "    datadog_agent_url http://localhost:8126;\n"
	nginxLoadModule       = "load_module /opt/datadog-nginx/ngx_http_datadog_module.so;\n"
)

// None of the directives inside the http block may be mandatory on removal. The
// installer's own comment tells users to delete datadog_tracing, and users delete
// the others for their own reasons; refusing to uninstall in any of those cases
// would make the documented workflow a dead end.
//
// Every case must still round-trip byte for byte, which is also what proves the
// comments the installer wrote are dropped on their own text: a comment left
// behind once the directive it documented is gone would otherwise survive and
// accrete on every install/uninstall cycle.
func TestRemoveDatadogFromHandEditedConfig(t *testing.T) {
	original, installed := installIntoBasicConfig(t)

	tests := []struct {
		name    string
		deleted []string
	}{
		{
			name:    "datadog_tracing deleted, its comment left behind",
			deleted: []string{nginxTracingDirective},
		},
		{
			name:    "datadog_tracing deleted with its comment",
			deleted: []string{nginxTracingComment, nginxTracingDirective},
		},
		{
			name:    "datadog_rum deleted, its comment left behind",
			deleted: []string{nginxRumDirective},
		},
		{
			name:    "both directives deleted, both comments left behind",
			deleted: []string{nginxTracingDirective, nginxRumDirective},
		},
		{
			name:    "datadog_agent_url deleted as redundant with the module default",
			deleted: []string{nginxAgentUrl},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			edited := installed
			for _, deleted := range tt.deleted {
				require.Contains(t, edited, deleted, "Failed to locate the line to delete")
				edited = strings.Replace(edited, deleted, "", 1)
			}

			transformed, err := removeDatadogFromConfig([]byte(edited))
			require.NoError(t, err, "Failed to remove Datadog from a hand-edited config")

			assert.Equal(t, original, string(transformed), "Hand-edited config did not round-trip")
		})
	}
}

// With every datadog_* directive gone by hand there is nothing left in the http
// block to remove, so removal reduces to dropping the load_module line. It has to
// succeed — that line is the only thing standing between the config and a modules
// directory that is about to be deleted — and it has to say what it did not find.
func TestRemoveDatadogFromConfigWithOnlyLoadModule(t *testing.T) {
	original, _ := installIntoBasicConfig(t)

	var warnings strings.Builder
	log.SetOutput(&warnings)
	defer log.SetOutput(os.Stderr)

	transformed, err := removeDatadogFromConfig([]byte(nginxLoadModule + original))
	require.NoError(t, err, "Failed to remove a bare load_module line")

	assert.Equal(t, original, string(transformed), "Removing a bare load_module line did not round-trip")
	assert.Assert(t, strings.Contains(warnings.String(), "No datadog_* directives were found"),
		"Expected a warning that nothing was found in the http block, got: %s", warnings.String())
}

// A duplicate load_module line passes 'nginx -t' while the module file is still
// there, so leaving one behind is only noticed once the directory is deleted and
// NGINX refuses to start. The duplicate may also name a different directory,
// which is still a reference to a datadog module we are removing.
func TestRemoveDatadogFromConfigWithMultipleLoadModules(t *testing.T) {
	original, installed := installIntoBasicConfig(t)

	tests := []struct {
		name      string
		duplicate string
	}{
		{name: "Same directory", duplicate: nginxLoadModule},
		{name: "Different directory", duplicate: "load_module /usr/lib/nginx/modules/ngx_http_datadog_module.so;\n"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			transformed, err := removeDatadogFromConfig([]byte(tt.duplicate + installed))
			require.NoError(t, err, "Failed to remove Datadog from a config with two load_module lines")

			assert.Equal(t, original, string(transformed), "Not every load_module line was removed")
		})
	}
}

// A load_module line below the http block used to make removal panic: the module
// offsets and the http block offsets were computed independently and then spliced
// together, so once the module sat after 'http {' the slice bounds crossed.
func TestRemoveDatadogFromConfigWithLoadModuleAfterHttpBlock(t *testing.T) {
	original, installed := installIntoBasicConfig(t)

	moved := strings.Replace(installed, nginxLoadModule, "", 1) + nginxLoadModule
	require.NotEqual(t, installed, moved, "Failed to move the load_module line below the http block")

	transformed, err := removeDatadogFromConfig([]byte(moved))
	require.NoError(t, err, "Failed to remove a load_module line placed after the http block")

	assert.Equal(t, original, string(transformed), "Config with a trailing load_module line did not round-trip")
}

func TestRemoveDatadogFromConfigErrors(t *testing.T) {
	original, installed := installIntoBasicConfig(t)

	tests := []struct {
		name          string
		content       string
		expectedError string
	}{
		{
			name:          "Config without the datadog module",
			content:       original,
			expectedError: "datadog load_module directive not found",
		},
		{
			name:          "Module loaded but no http block",
			content:       "load_module /opt/datadog-nginx/ngx_http_datadog_module.so;\nevents {\n}\n",
			expectedError: "could not find http block",
		},
		{
			name:          "datadog_rum_config block never closes",
			content:       strings.Replace(installed, "\n    }\n", "\n", 1),
			expectedError: "malformed",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := removeDatadogFromConfig([]byte(tt.content))

			require.Error(t, err, "Expected removal to fail")
			assert.Assert(t, strings.Contains(err.Error(), tt.expectedError),
				"Expected error containing %q, got %q", tt.expectedError, err.Error())
		})
	}
}

// Deleting the datadog_rum_config block by hand must not block the uninstall
// either. The blank line it was written with is the user's to keep, so this is the
// one hand-edit that does not round-trip byte for byte — but every directive the
// installer wrote still has to go.
func TestRemoveDatadogFromConfigWithoutRumConfigBlock(t *testing.T) {
	_, installed := installIntoBasicConfig(t)

	transformed, err := removeDatadogFromConfig([]byte(removeRumConfigBlock(t, installed)))
	require.NoError(t, err, "Failed to remove Datadog from a config missing its datadog_rum_config block")

	assert.Assert(t, !strings.Contains(string(transformed), "datadog"),
		"Datadog directives survived removal: %s", string(transformed))
}

// Strips just the datadog_rum_config block, leaving the other directives behind.
func removeRumConfigBlock(t *testing.T, installed string) string {
	t.Helper()

	start := strings.Index(installed, `    datadog_rum_config "v6" {`)
	require.NotEqual(t, -1, start, "Failed to locate the datadog_rum_config block")

	end := strings.Index(installed[start:], "\n    }\n")
	require.NotEqual(t, -1, end, "Failed to locate the end of the datadog_rum_config block")

	return installed[:start] + installed[start+end+len("\n    }\n"):]
}

func TestRemoveDatadogFromHttpdConfig(t *testing.T) {
	const original = "ServerRoot \"/etc/httpd\"\nListen 80\n"
	installed := fmt.Sprintf("%s\nInclude %s/datadog.conf\n", original, InstallPath)

	t.Run("Round-trips an installed config", func(t *testing.T) {
		transformed, err := removeDatadogFromHttpdConfig([]byte(installed))
		require.NoError(t, err, "Failed to remove Datadog from httpd config")

		assert.Equal(t, original, string(transformed), "httpd config did not round-trip")
	})

	t.Run("Keeps unrelated Include lines", func(t *testing.T) {
		withOther := fmt.Sprintf("%sInclude conf.modules.d/*.conf\nInclude %s/datadog.conf\n", original, InstallPath)

		transformed, err := removeDatadogFromHttpdConfig([]byte(withOther))
		require.NoError(t, err, "Failed to remove Datadog from httpd config")

		assert.Equal(t, original+"Include conf.modules.d/*.conf\n", string(transformed),
			"Removal did not leave unrelated Include lines alone")
	})

	// A duplicate Include passes 'httpd -t' while datadog.conf is still there, so
	// leaving one behind is only noticed once the installation directory is gone
	// and Apache refuses to start on a missing required file.
	t.Run("Removes every occurrence", func(t *testing.T) {
		duplicated := fmt.Sprintf("%s\nInclude %s/datadog.conf\nInclude conf.modules.d/*.conf\n\tInclude %s/datadog.conf\n",
			original, InstallPath, InstallPath)

		transformed, err := removeDatadogFromHttpdConfig([]byte(duplicated))
		require.NoError(t, err, "Failed to remove Datadog from httpd config")

		assert.Equal(t, original+"Include conf.modules.d/*.conf\n", string(transformed),
			"Not every datadog Include line was removed")
	})

	t.Run("Fails when the module is not installed", func(t *testing.T) {
		_, err := removeDatadogFromHttpdConfig([]byte(original))

		require.Error(t, err, "Expected removal to fail on a config without the module")
		assert.Assert(t, strings.Contains(err.Error(), "does not appear to be installed"),
			"Unexpected error: %v", err)
	})
}

func TestHttpdConfigFile(t *testing.T) {
	tests := []struct {
		name                    string
		agentUri                string
		appID                   string
		clientToken             string
		site                    string
		sessionSampleRate       int
		sessionReplaySampleRate int
		remoteConfigurationId   string
		expected                []string
	}{
		{
			name:                    "With local configuration",
			agentUri:                "http://localhost:8126",
			appID:                   "ffffffff-ffff-ffff-ffff-ffffffffffff",
			clientToken:             "pubffffffffffffffffffffffffffffffff",
			site:                    "datadoghq.com",
			sessionSampleRate:       98,
			sessionReplaySampleRate: 99,
			expected: []string{
				`LoadModule datadog_module /opt/datadog-httpd/mod_datadog.so`,
				"DatadogTracing Off",
				"DatadogRum On",
				`DatadogAgentUrl "http://localhost:8126"`,
				"<DatadogRumSettings \"v6\">",
				`DatadogRumOption "applicationId" "ffffffff-ffff-ffff-ffff-ffffffffffff"`,
				`DatadogRumOption "clientToken" "pubffffffffffffffffffffffffffffffff"`,
				`DatadogRumOption "site" "datadoghq.com"`,
				`DatadogRumOption "sessionSampleRate" "98"`,
				`DatadogRumOption "sessionReplaySampleRate" "99"`,
				"</DatadogRumSettings>",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			res := datadogConfigFile(
				tt.remoteConfigurationId,
				tt.agentUri,
				tt.appID,
				tt.clientToken,
				tt.site,
				tt.sessionSampleRate,
				tt.sessionReplaySampleRate,
			)

			resLines := strings.Split(strings.TrimSpace(res), "\n")

			if len(resLines) != len(tt.expected) {
				t.Errorf("generated %d lines, expected %d lines", len(resLines), len(tt.expected))
				return
			}

			for i := 0; i < len(resLines); i++ {
				resLine := strings.TrimSpace(resLines[i])
				expectedLine := strings.TrimSpace(tt.expected[i])
				if resLine != expectedLine {
					t.Errorf("line %d:\result:  %s\nexpected: %s", i, resLine, expectedLine)
				}
			}
		})
	}
}
