$ErrorActionPreference = 'Stop'
$started = [DateTime]::UtcNow
$application = 'RustDeskNativeStop' + [Guid]::NewGuid().ToString('N')
$brokerName = $application + 'Broker.exe'
$root = Join-Path $env:RUNNER_TEMP $application
$helper = (Resolve-Path './rustdesk/rustdesk.exe').Path
if ((Get-AuthenticodeSignature -LiteralPath $helper).Status -ne 'Valid') { throw 'Native runner must be signed' }
$fixture = (Resolve-Path './target/native-service-stop/fixture.exe').Path
$shortcut = Join-Path ([Environment]::GetFolderPath('CommonStartup')) "$application Tray.lnk"
$children = @()
$serviceCreated = $false
New-Item -ItemType Directory -Path $root | Out-Null
function Invoke-NativeStop([string[]] $Arguments, [int] $Expected) {
  $out = Join-Path $root 'stdout.txt'
  $err = Join-Path $root 'stderr.txt'
  $options = @{ FilePath=$helper; WindowStyle='Hidden'; RedirectStandardOutput=$out; RedirectStandardError=$err; PassThru=$true }
  if ($Arguments.Count) { $options.ArgumentList = $Arguments }
  $child = Start-Process @options
  if (-not $child.WaitForExit(30000)) { Stop-Process -Id $child.Id; throw 'Native helper exceeded 30 seconds' }
  Get-Content -LiteralPath $out, $err
  if ($child.ExitCode -ne $Expected) { throw "Expected exit $Expected, got $($child.ExitCode)" }
}
try {
  Invoke-NativeStop -Arguments @('--native-service-stop') -Expected 160
  Copy-Item -LiteralPath $fixture -Destination (Join-Path $root "$application.exe")
  Copy-Item -LiteralPath $fixture -Destination (Join-Path $root $brokerName)
  foreach ($name in @("$application.exe", "$application.exe", $brokerName)) {
    $children += Start-Process -FilePath (Join-Path $root $name) -ArgumentList '--idle' -WindowStyle Hidden -PassThru
  }
  [IO.File]::WriteAllText($shortcut, 'Temporary native-stop test shortcut')
  (Get-Item -LiteralPath $shortcut).IsReadOnly = $true
  & sc.exe create $application 'binPath=' ('"' + $fixture + '" --service') 'start=' 'demand'
  if ($LASTEXITCODE -ne 0) { throw 'Could not create fixture service' }
  $serviceCreated = $true
  & sc.exe start $application
  if ($LASTEXITCODE -ne 0) { throw 'Could not start fixture service' }
  $service = Get-Service -Name $application
  try { $service.WaitForStatus('Running', [TimeSpan]::FromSeconds(10)) } finally { $service.Dispose() }
  $arguments = @('--native-service-stop', $application, $brokerName, [string]$children[0].Id)
  Invoke-NativeStop -Arguments $arguments -Expected 0
  & sc.exe query $application
  if ($LASTEXITCODE -ne 1060) { throw 'Fixture service was not removed' }
  if (Test-Path -LiteralPath $shortcut) { throw 'Startup shortcut was not removed' }
  if ($children[0].HasExited) { throw 'The caller was terminated' }
  foreach ($child in $children[1..2]) {
    if (-not $child.WaitForExit(1000)) { throw 'Matching app or broker process remains' }
  }
  Invoke-NativeStop -Arguments $arguments -Expected 0
  if ($children[0].HasExited) { throw 'Repeated stop terminated the caller' }
  New-Item -ItemType Directory -Path './SignOutput' -Force | Out-Null
  [ordered]@{
    passed=$true; service_removed=$true; readonly_shortcut_removed=$true
    app_and_broker_terminated=$true; caller_preserved=$true; repeated_stop_passed=$true
    invalid_arguments_exit=160; elapsed_seconds=([DateTime]::UtcNow - $started).TotalSeconds
    scope='Real disposable-runner fixtures; actual signed runner native entry; no F-Secure or UAC consent test'
  } | ConvertTo-Json | Set-Content -LiteralPath "./SignOutput/$env:TEST_PACKAGE.native-stop-test.json" -Encoding utf8
} finally {
  foreach ($child in $children) {
    if (-not $child.HasExited) { Stop-Process -Id $child.Id }
    $child.Dispose()
  }
  if ($serviceCreated) {
    & sc.exe stop $application
    if ($LASTEXITCODE -notin @(0, 1060, 1062)) { Write-Error 'Fixture service cleanup stop failed' }
    & sc.exe delete $application
    if ($LASTEXITCODE -notin @(0, 1060, 1072)) { Write-Error 'Fixture service cleanup delete failed' }
  }
  if (Test-Path -LiteralPath $shortcut) { Remove-Item -LiteralPath $shortcut -Force }
}
exit 0
