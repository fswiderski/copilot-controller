$ErrorActionPreference = 'Stop'

if ($env:RUNNER_OS -ne 'Windows') {
    throw 'Windows roots require a Windows runner'
}
$workspace = $env:GITHUB_WORKSPACE
if (-not [IO.Path]::IsPathFullyQualified($workspace) -or $workspace -notmatch '^[A-Za-z]:\\') {
    throw 'Workspace must be an absolute drive-letter path'
}
$parent = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($workspace))
$drive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($parent))
if (-not $drive.IsReady -or $drive.DriveType -ne [IO.DriveType]::Fixed) {
    throw 'Fixture filesystem must be on a fixed local drive'
}
for ($part = Get-Item -LiteralPath $parent -Force; $null -ne $part; $part = $part.Parent) {
    if (-not ($part.Attributes -band [IO.FileAttributes]::Directory) -or
        ($part.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Unsafe or reparse-point fixture ancestor: $($part.FullName)"
    }
}
if ($env:GITHUB_RUN_ID -notmatch '^[0-9]+$' -or $env:GITHUB_RUN_ATTEMPT -notmatch '^[0-9]+$') {
    throw 'Invalid run ID or attempt'
}

$root = Join-Path $parent ".controller-ci-$env:GITHUB_RUN_ID-$env:GITHUB_RUN_ATTEMPT"
New-Item -ItemType Directory -Path $root | Out-Null
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
$acl = [Security.AccessControl.DirectorySecurity]::new()
$acl.SetOwner($sid)
$acl.SetAccessRuleProtection($true, $false)
$inheritance = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor `
    [Security.AccessControl.InheritanceFlags]::ObjectInherit
$rule = [Security.AccessControl.FileSystemAccessRule]::new(
    $sid,
    [Security.AccessControl.FileSystemRights]::FullControl,
    $inheritance,
    [Security.AccessControl.PropagationFlags]::None,
    [Security.AccessControl.AccessControlType]::Allow
)
$acl.AddAccessRule($rule)
Set-Acl -LiteralPath $root -AclObject $acl
$actual = Get-Acl -LiteralPath $root
$rules = @($actual.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
if (-not $actual.AreAccessRulesProtected -or $rules.Count -ne 1 -or
    $rules[0].IdentityReference.Value -ne $sid.Value -or
    $rules[0].AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow -or
    ($rules[0].FileSystemRights -band [Security.AccessControl.FileSystemRights]::FullControl) -ne
        [Security.AccessControl.FileSystemRights]::FullControl -or
    $rules[0].InheritanceFlags -ne $inheritance -or
    $actual.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $sid.Value) {
    throw 'Fixture root does not have a private owner-only ACL'
}

$target = Join-Path $root 'target'
$fixtures = Join-Path $root 'fixtures'
New-Item -ItemType Directory -Path $target, $fixtures | Out-Null
@(
    "CARGO_TARGET_DIR=$target"
    "COPILOT_CONTROLLER_TEST_ROOT=$fixtures"
    "COPILOT_PRIVATE_ROOT_FIXTURES=$fixtures"
    "COPILOT_FIRMWARE_FIXTURE_ROOT=$fixtures"
    "PORTABLE_TEST_FIXTURES=$fixtures"
    "SESSION_ENGINE_SAFE_FIXTURE_ROOT=$fixtures"
    "TMPDIR=$fixtures"
    "TMP=$fixtures"
    "TEMP=$fixtures"
) | Out-File -LiteralPath $env:GITHUB_ENV -Encoding utf8 -Append
Write-Output "Fixture filesystem: $($drive.DriveFormat) on a fixed drive"
Write-Output "Build root: $target"
Write-Output "Private fixture root: $fixtures"
