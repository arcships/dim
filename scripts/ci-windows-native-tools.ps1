$ProgressPreference = 'SilentlyContinue'
function Add-PortableTool {
  param([string]$Name, [string]$Version, [string]$Sha256, [string]$Url, [string]$VersionFlag)
  if (Get-Command $Name -ErrorAction SilentlyContinue) {
    Write-Output "Using the $Name already on PATH"
    return
  }
  $archive = Join-Path $env:RUNNER_TEMP "$Name-$Version.zip"
  Invoke-WebRequest -Uri $Url -OutFile $archive -UseBasicParsing
  $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
  if ($actual -ne $Sha256) { throw "$Name sha256 $actual does not match $Sha256" }
  $directory = Join-Path $env:RUNNER_TEMP "dim-windows-native-$Name-$Version"
  if (Test-Path $directory) { Remove-Item -LiteralPath $directory -Recurse -Force }
  Expand-Archive -LiteralPath $archive -DestinationPath $directory -Force
  $executable = Get-ChildItem -LiteralPath $directory -Recurse -File -Filter "$Name.exe" | Select-Object -First 1
  if (-not $executable) { throw "$Name archive holds no $Name.exe" }
  [System.IO.File]::AppendAllText($env:GITHUB_PATH, "$($executable.DirectoryName)`n")
  & $executable.FullName $VersionFlag
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  Write-Output "Provisioned $Name $Version from $Url"
}
Add-PortableTool -Name 'cmake' -Version $env:CMAKE_VERSION -Sha256 $env:CMAKE_SHA256 -Url "https://github.com/Kitware/CMake/releases/download/v$env:CMAKE_VERSION/cmake-$env:CMAKE_VERSION-windows-x86_64.zip" -VersionFlag '--version'
Add-PortableTool -Name 'nasm' -Version $env:NASM_VERSION -Sha256 $env:NASM_SHA256 -Url "https://www.nasm.us/pub/nasm/releasebuilds/$env:NASM_VERSION/win64/nasm-$env:NASM_VERSION-win64.zip" -VersionFlag '-v'
