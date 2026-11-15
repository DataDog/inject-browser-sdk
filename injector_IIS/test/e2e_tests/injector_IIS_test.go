// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

package e2etests

import (
  "fmt"
  runneros "os"
  "path"
  "path/filepath"
  "runtime"
  "strconv"
  "strings"
  "testing"

  "github.com/DataDog/datadog-agent/test/e2e-framework/testing/e2e"
  "github.com/DataDog/datadog-agent/test/e2e-framework/testing/environments"
  awsHostWindows "github.com/DataDog/datadog-agent/test/e2e-framework/testing/provisioners/aws/host/windows"
  "github.com/DataDog/datadog-agent/test/new-e2e/tests/windows"
  windowsCommon "github.com/DataDog/datadog-agent/test/new-e2e/tests/windows/common"
  commonAgent "github.com/DataDog/datadog-agent/test/new-e2e/tests/windows/common/agent"


  "github.com/stretchr/testify/require"
)

var (
  assetDir     string
)

type rumvmSuite struct {
  e2e.BaseSuite[environments.WindowsHost]
  isUpgradeSuite bool
  latestVersion []string
}

type rumInstallSuite struct {
  rumvmSuite
}

type rumUpgradeSuite struct {
  rumvmSuite
}
var (
  // this is currently hard-coded to a well known location/version.  Once releases have been made, need to update to
  // dynamically pick up the "last known good" version.
  oldVersionForUpgrade = "https://s3.amazonaws.com/ddagent-windows-unstable/inject-browser-sdk/upgrade/injector_iis-0.0.2.msi"
  oldVersionValidation = "0.0.2"
  // What customers download today: the last release promoted by .gitlab/release.yml.
  latestReleaseMSI = "https://rum-auto-instrumentation.s3.amazonaws.com/iis/latest/x64/injector_IIS.msi"
  // Releases before this MAJ.MIN may remove the RUM configuration when a
  // major upgrade uninstalls them (fixed by #145, which keeps it on upgrade).
  firstConfigKeepingRelease = []string{"1", "4"}
)
var sitesToTest = []windows.IISSiteDefinition{
  {
    Name: "default_site", // tests simple injection
    BindingPort: "*:8080:",
  },
  {
    Name: "large_site", // tests injection into large response
    BindingPort: "*:8081:",
  },
  {
    Name: "sitewithapps", // tests injection into a site with multiple applications
    BindingPort: "*:8082:",
    SiteDir:     path.Join("c:", "site1"),
    Applications: []windows.IISApplicationDefinition{
      {
        Name:         "/site1/app1",
        PhysicalPath: path.Join("c:", "app1"),
      },
      {
        Name:         "/site1/app2",
        PhysicalPath: path.Join("c:", "app2"),
      },
      {
        Name:         "/site1/app2/nested",
        PhysicalPath: path.Join("c:", "app2", "nested"),
      },
    },
  },
  {
    Name: "location_site", // tests <location> override disabling RUM for a sub-path
    BindingPort: "*:8083:",
  },
}
type injectionTest struct {
  url string
  injectionService string
  title string
  allowedTracingUrls []string
}
var expected_results = []injectionTest {
  {
    url: "http://localhost:8080",
    injectionService: `"service":"my-web-application"`,
    title: "simple injection test, default_site",
    allowedTracingUrls: []string{
      "https://first.myapp.com",
      "https://second.myapp.com",
    },
  },
  {
    url: "http://localhost:8081",
    injectionService: `"service":"my-web-application"`,
    title: "large file injection test, large_site",
    allowedTracingUrls: nil,
  },
  {
    url: "http://localhost:8082",
    injectionService: "",  // no injection set up on this site
    title: "no injection test, sitewithapps",
    allowedTracingUrls: nil,
  },
  {
    url: "http://localhost:8082/site1/app1",
    injectionService: `"service":"app1"`,
    title: "simple app injection test, sitewithapps/app1",
    allowedTracingUrls: nil,
  },
  {
    url: "http://localhost:8082/site1/app2",
    injectionService: `"service":"app2"`,
    title: "simple app injection test, sitewithapps/app2",
    allowedTracingUrls: nil,
  },
  {
    url: "http://localhost:8082/site1/app2/nested",
    injectionService: `"service":"nested"`,
    title: "deep app injection test, sitewithapps/app2/nested",
    allowedTracingUrls: nil,
  },
  {
    url: "http://localhost:8083",
    injectionService: `"service":"location-test"`,
    title: "location override test, root path should inject",
    allowedTracingUrls: nil,
  },
  {
    url: "http://localhost:8083/api/",
    injectionService: "",  // disabled via <location path="api">
    title: "location override test, /api path should NOT inject",
    allowedTracingUrls: nil,
  },
}

func TestRUMInstallSuite(t *testing.T) {
  suiteParams := []e2e.SuiteOption{e2e.WithProvisioner(awsHostWindows.ProvisionerNoAgentNoFakeIntake())}

  e2e.Run(t, &rumInstallSuite{}, suiteParams...)
}
func TestRUMUpgradeSuite(t *testing.T) {
  suiteParams := []e2e.SuiteOption{e2e.WithProvisioner(awsHostWindows.ProvisionerNoAgentNoFakeIntake())}

  suite := &rumUpgradeSuite{}
  suite.isUpgradeSuite = true
  
  e2e.Run(t, suite, suiteParams...)
}

func (v *rumvmSuite) SetupSuite() {
  t := v.T()

  currDir, _ := runneros.Getwd()
  assetDir = filepath.Join(currDir, "test_assets")

  // this creates the VM.
  v.BaseSuite.SetupSuite()

  // get the remote host
  vm := v.Env().RemoteHost

  // configure the assets dir
  for idx := range sitesToTest {
    sitesToTest[idx].AssetsDir = filepath.Join(assetDir, sitesToTest[idx].Name)
  }

  err := windows.InstallIIS(vm)
  require.NoError(t, err)
  // HEADSUP the paths are windows, but this will execute in linux. So fix the paths
  t.Log("IIS Installed, continuing")

  err = windows.CreateIISSite(vm, sitesToTest)
  v.Require().NoError(err)

  if !v.isUpgradeSuite {
    v.copyApplicationConfigs()
  } else {
    v.copyOldVersinConfigFiles()
  }

  v.T().Log("Restarting IIS server")
  vm.Execute("iisreset /restart")
}

// requireRun is an additional wrapper so that the test fails immediately on failure
// of a subtest function, rather than continuing on.
func (v *rumvmSuite) requireRun(desc string, f func ()) {
  if !v.Run(desc, f) {
    v.FailNow("Failed to successfully run subtest %s", desc)
  }
}

// createTestOutputDir replaces the now-removed e2e.BaseSuite.CreateTestOutputDir.
// e2e-framework only exposes SessionOutputDir; build the per-test subdirectory
// ourselves so install logs from each test stay separate.
func (v *rumvmSuite) createTestOutputDir() (string, error) {
  dir := filepath.Join(v.SessionOutputDir(), v.T().Name())
  if err := runneros.MkdirAll(dir, 0o755); err != nil {
    return "", err
  }
  return dir, nil
}
func (v *rumInstallSuite) TestInjectorIISInstall() {

  v.requireRun("Install latest release", v.installLatestRelease)
  v.requireRun("Upgrade from latest release", v.testInstall)
  v.requireRun("RUM configuration after upgrade", v.testConfigAfterUpgrade)
  v.requireRun("Test injection after upgrade", v.testSuccessfulInjectionAll)
  v.requireRun("Uninstall upgraded", v.testUninstall)
  v.requireRun("Restore RUM configuration removed by uninstall", v.restoreRUMConfig)

  v.requireRun("Install", v.testInstall)
  v.requireRun("Test injection", v.testSuccessfulInjectionAll)
  v.requireRun("Downgrade to latest release is refused", v.testDowngradeRefused)
  v.requireRun("Uninstall", v.testUninstall)

}
func (v *rumUpgradeSuite) TestInjectorIISUpgrade() {
  
  v.requireRun("Install old version for upgrade", v.installOldVersionForUpgrade)
  v.requireRun("Upgrade", v.testInstall)
  v.requireRun("Test injection after migration", v.testSuccessfulInjectionAll)
  v.requireRun("Uninstall", v.testUninstall)
}

func (v *rumvmSuite) testInstall() {
  vm := v.Env().RemoteHost

  // Copy the injector installer to the VM
  remoteFilePath := filepath.Join(remoteInstallerPath, installerName)
  iname := filepath.Join(assetDir, installerName)
  v.T().Logf("Attempting to copy %s to %s", iname, remoteFilePath)
  if _, err := runneros.Stat(iname); err != nil {
    v.T().Logf("Didn't find %s %v", iname, err)
  } else {
    v.T().Logf("Found %s", iname)
  }
  vm.CopyFile(iname, remoteFilePath)
  // The build verifies MSI/DLL metadata against the pipeline IID and writes
  // this expectation alongside the artifacts (not from the installed DLL).
  // Only release tag pipelines sign (IIS_SIGN); check signatures there.
  v.doTestInstall(remoteFilePath, v.expectedVersion(), runneros.Getenv("IIS_SIGN") != "1")
}

// expectedVersion is the MAJ.MIN.BUILD the build recorded for its artifacts.
func (v *rumvmSuite) expectedVersion() string {
  version, err := runneros.ReadFile(filepath.Join(assetDir, "injector_IIS.version"))
  v.Require().NoError(err)
  expectedVersion := strings.TrimSpace(string(version))
  v.Require().Regexp(`^[0-9]+\.[0-9]+\.[0-9]+$`, expectedVersion)
  return expectedVersion
}

// newerThan reports whether MAJ.MIN.BUILD a sorts after b, comparing the three
// fields Windows Installer uses for upgrades.
func newerThan(a, b []string) bool {
  for i := 0; i < 3; i++ {
    x, errX := strconv.Atoi(a[i])
    y, errY := strconv.Atoi(b[i])
    if errX != nil || errY != nil || x == y {
      continue
    }
    return x > y
  }
  return false
}

// candidateIsNewerThanLatest is false only for local builds (BUILD 0), which
// cannot upgrade a released MSI; in CI every pipeline IID is larger.
func (v *rumvmSuite) candidateIsNewerThanLatest(latest []string) bool {
  if newerThan(strings.Split(v.expectedVersion(), "."), latest) {
    return true
  }
  v.Require().Empty(runneros.Getenv("CI"), "build %s is not newer than the latest release %v", v.expectedVersion(), latest)
  return false
}

// copyApplicationConfigs copies the applications' web.config files; the sites'
// own come with their assets.
func (v *rumvmSuite) copyApplicationConfigs() {
  vm := v.Env().RemoteHost
  // NOTE(@dmehala): The agent code is wrong for application
  for _, site := range sitesToTest {
    // copy the configs, if they exist, for the applications
    for _, app := range site.Applications {
      localConfigFilePath := filepath.Join(assetDir, site.Name, app.Name, "web.config")
      if _, err := runneros.Stat(localConfigFilePath); err != nil {
        v.T().Logf("Didn't find %s %v, skipping", localConfigFilePath, err)
      } else {
        v.T().Logf("Found %s", localConfigFilePath)
        remoteFilePath := filepath.Join(app.PhysicalPath, "web.config")
        vm.CopyFile(localConfigFilePath, remoteFilePath)
      }
    }
  }
}

// testConfigAfterUpgrade checks the upgrade kept the RUM configuration.
// Releases before firstConfigKeepingRelease (including 1.3.x patches without
// #145) may remove it: restore it then, so the following steps still check
// that the upgraded module injects.
func (v *rumvmSuite) testConfigAfterUpgrade() {
  vm := v.Env().RemoteHost
  headers := vm.MustExecute("(iwr -UseBasicParsing " + expected_results[0].url + ").Headers")
  if strings.Contains(headers, "x-datadog-rum-injected") {
    return
  }
  v.Require().NotNil(v.latestVersion, "latest release version was not recorded")
  v.Require().True(newerThan(append(firstConfigKeepingRelease, "0"), v.latestVersion),
    "upgrading from %v removed the RUM configuration", v.latestVersion)
  v.T().Logf("Known issue: upgrading from %v removed the RUM configuration; restoring it", v.latestVersion)
  v.restoreRUMConfig()
}

// restoreRUMConfig puts back the sites' and applications' RUM configuration,
// which an uninstall removes.
func (v *rumvmSuite) restoreRUMConfig() {
  vm := v.Env().RemoteHost
  for _, site := range sitesToTest {
    localConfigFilePath := filepath.Join(assetDir, site.Name, "web.config")
    if _, err := runneros.Stat(localConfigFilePath); err != nil {
      continue
    }
    siteDir := site.SiteDir
    if siteDir == "" {
      siteDir = path.Join("c:", "tmp", "inetpub", site.Name)
    }
    vm.CopyFile(localConfigFilePath, filepath.Join(siteDir, "web.config"))
  }
  v.copyApplicationConfigs()
  vm.MustExecute("iisreset /restart")
}

// installLatestRelease installs the MSI customers get today, so the following
// install step tests the upgrade they will run.
func (v *rumvmSuite) installLatestRelease() {
  vm := v.Env().RemoteHost
  outputDir, err := v.createTestOutputDir()
  v.Require().NoError(err)
  err = windowsCommon.InstallMSI(vm, latestReleaseMSI, "IISRESTART=true", filepath.Join(outputDir, "install.log"))
  v.Require().NoError(err)
  latest, err := windowsCommon.GetFileVersion(vm, defaultBinPath)
  v.Require().NoError(err)
  v.T().Logf("Latest release installed version %v", latest)
  v.latestVersion = latest
  if !v.candidateIsNewerThanLatest(latest) {
    err = windowsCommon.UninstallMSI(vm, latestReleaseMSI, "IISRESTART=true", filepath.Join(outputDir, "uninstall.log"))
    v.Require().NoError(err)
    v.T().Skip("local build cannot upgrade the latest release; installing fresh instead")
  }
}

// testDowngradeRefused checks MajorUpgrade blocks installing the older latest
// release over this build and leaves this build in place.
func (v *rumvmSuite) testDowngradeRefused() {
  vm := v.Env().RemoteHost
  v.Require().NotNil(v.latestVersion, "latest release version was not recorded")
  if !v.candidateIsNewerThanLatest(v.latestVersion) {
    v.T().Skip("local build is not newer than the latest release")
  }
  outputDir, err := v.createTestOutputDir()
  v.Require().NoError(err)
  err = windowsCommon.InstallMSI(vm, latestReleaseMSI, "IISRESTART=true", filepath.Join(outputDir, "downgrade.log"))
  v.Require().Error(err, "installing an older release over %s should fail", v.expectedVersion())
  v.Require().NoError(windowsCommon.VerifyVersion(vm, defaultBinPath, v.expectedVersion()))
}

func (v *rumvmSuite) installOldVersionForUpgrade() {
  vm := v.Env().RemoteHost

  // Run the installer
  if runtime.GOOS != "windows" {
    // because oldVersionForUpgrade is constructed at runtime, we have to convert it to a windows path if the test runner is linux
    oldVersionForUpgrade = strings.ReplaceAll(oldVersionForUpgrade, "/", "\\")
  }
  outputDir, err := v.createTestOutputDir()
  v.Require().NoError(err)
  err = windowsCommon.InstallMSI(vm, oldVersionForUpgrade, "IISRESTART=true", filepath.Join(outputDir, "install.log"))
  v.Require().NoError(err)
  // Verify installer put the injector DLL in the right place
  v.T().Logf("Checking for injector DLL at '%s'\n", defaultBinPath)
  res := strings.TrimSpace(vm.MustExecute(fmt.Sprintf("Test-Path -Path '%s'", defaultBinPath)))
  v.Require().Equal("True", res)

  // Verify the injector DLL has the right version
  err = windowsCommon.VerifyVersion(vm, defaultBinPath, oldVersionValidation)
  if err != nil {
    v.T().Logf("Failed to get version on %s: %v", defaultBinPath, err)
  }
  v.Require().NoError(err)

  // Verify the injector DLL has the right permissions
  v.T().Logf("Checking injector DLL permissions at '%s'\n", defaultBinPath)
  accessMatches, err := isBasicFileAccessEqual(vm, defaultBinPath, expectedBinAccess)
  v.Require().NoError(err)
  v.Require().True(accessMatches)
}

func (v *rumvmSuite) doTestInstall(remoteFilePath string, expectedVersion string, skipSigCheck bool) {
  vm := v.Env().RemoteHost

  // Run the installer
  if runtime.GOOS != "windows" {
    // because remoteFilePath is constructed at runtime, we have to convert it to a windows path if the test runner is linux
    remoteFilePath = strings.Replace(remoteFilePath, "/", "\\", -1)
  }
  outputDir, err := v.createTestOutputDir()
  v.Require().NoError(err)
  err = windowsCommon.InstallMSI(vm, remoteFilePath, "IISRESTART=true", filepath.Join(outputDir, "install.log"))
  v.Require().NoError(err)

  // Verify installer put the injector DLL in the right place
  v.T().Logf("Checking for injector DLL at '%s'\n", defaultBinPath)
  res := strings.TrimSpace(vm.MustExecute(fmt.Sprintf("Test-Path -Path '%s'", defaultBinPath)))
  v.Require().Equal("True", res)

  // Verify the injector DLL has the right version
  err = windowsCommon.VerifyVersion(vm, defaultBinPath, expectedVersion)
  if err != nil {
    v.T().Logf("Failed to get version on %s: %v", defaultBinPath, err)
  }
  v.Require().NoError(err)

  // Verify the injector DLL has the right permissions
  v.T().Logf("Checking injector DLL permissions at '%s'\n", defaultBinPath)
  accessMatches, err := isBasicFileAccessEqual(vm, defaultBinPath, expectedBinAccess)
  v.Require().NoError(err)
  v.Require().True(accessMatches)

  if !skipSigCheck {
    sigcheck := []string{remoteFilePath}
    signed := commonAgent.TestValidDatadogCodeSignatures(v.T(), vm, sigcheck)
    v.Require().True(signed)
  }
  
  // Verify the installer put the injector config in the right place
  v.T().Logf("Checking for injector config schema at '%s'\n", configSchemaPath)
  res = strings.TrimSpace(vm.MustExecute(fmt.Sprintf("Test-Path -Path '%s'", configSchemaPath)))
  v.Assert().Equal("True", res)

  // Verify the injector config has the right permissions
  // v.T().Logf("Checking injector config permissions at '%s'\n", exampleConfigPath)
  // accessMatches, err = isBasicFileAccessEqual(vm, exampleConfigPath, expectedConfigAccess)
  // v.Require().NoError(err)
  // v.Require().True(accessMatches)
}

func (v *rumvmSuite) testSuccessfulInjectionAll() {
  v.testSuccessfulInjection(expected_results)
}

func (v *rumvmSuite) testSuccessfulInjection(tests []injectionTest) {
  vm := v.Env().RemoteHost

  for _, test := range tests {

    v.Run(test.title, func() {

      v.T().Logf("Testing injection for %s", test.url)
      response := vm.MustExecute("(iwr -UseBasicParsing " + test.url + ").Content")
      
      responseHeaders := vm.MustExecute("(iwr -UseBasicParsing " + test.url + ").Headers")
      
      if test.injectionService != "" {
        v.Assert().Contains(responseHeaders, "x-datadog-rum-injected")
        v.Assert().Contains(response, test.injectionService)

        if test.allowedTracingUrls != nil && len(test.allowedTracingUrls) > 0 {
          quotedUrls := make([]string, len(test.allowedTracingUrls))
          for i, url := range test.allowedTracingUrls {
            quotedUrls[i] = fmt.Sprintf(`"%s"`, url)
          }
          expectedAllowedTracingUrls := fmt.Sprintf(`"allowedTracingUrls":[%s]`, strings.Join(quotedUrls, ","))

          v.Assert().Contains(response, expectedAllowedTracingUrls)
          v.T().Logf("Verified allowedTracingUrls in injection: %s", expectedAllowedTracingUrls)
        }
      } else {
        v.Assert().NotContains(responseHeaders, "x-datadog-rum-injected")
        v.Assert().NotContains(response, `"service":`)
      }
    })
  }
}

func (v *rumvmSuite) testUninstall() {
  vm := v.Env().RemoteHost

  // Uninstall the injector
  outputDir, err := v.createTestOutputDir()
  v.Require().NoError(err)

  remotePath := filepath.Join(remoteInstallerPath, installerName)
  err = windowsCommon.UninstallMSI(vm, remotePath, "IISRESTART=true", filepath.Join(outputDir,"uninstall.log"))
  v.Require().NoError(err)
  
  // Verify the injector DLL is gone
  res := strings.TrimSpace(vm.MustExecute(fmt.Sprintf("Test-Path -Path '%s'", defaultBinPath)))
  v.Assert().Equal("False", res)

  // Verify web server serves correctly without injection
  noInjectionExpected := make([]injectionTest, len(expected_results))
  for i, test := range expected_results {
    noInjectionExpected[i] = injectionTest{
      url:             test.url,
      injectionService: "", // No injection expected
      title:           test.title,
    }
  }

  v.testSuccessfulInjection(noInjectionExpected)
}

func (v *rumvmSuite) copyOldVersinConfigFiles() {
  t := v.T()
  vm := v.Env().RemoteHost

  var configFiles []string
  err := filepath.Walk(assetDir, func(path string, info runneros.FileInfo, err error) error {
    if err != nil {
      return err
    }
    if !info.IsDir() && filepath.Base(path) == "test-config.json" {
      configFiles = append(configFiles, path)
    }
    return nil
  })
  
  if err != nil {
    t.Logf("Error finding config files: %v", err)
    return
  }

  for _, sourceFile := range configFiles {
    relPath, err := filepath.Rel(assetDir, sourceFile)
    if err != nil {
      t.Logf("Error calculating relative path for %s: %v", sourceFile, err)
      continue
    }

    destDir := filepath.Join(defaultConfigDir, filepath.Dir(relPath))
    destFile := filepath.Join(destDir, "rum-config.json")

    winDestDir := strings.ReplaceAll(destDir, "/", "\\")
    winDestFile := strings.ReplaceAll(destFile, "/", "\\")

    vm.MustExecute(fmt.Sprintf("New-Item -ItemType Directory -Force -Path '%s'", winDestDir))

    t.Logf("Copying %s to %s", sourceFile, winDestFile)
    vm.CopyFile(sourceFile, winDestFile)
  }
}
