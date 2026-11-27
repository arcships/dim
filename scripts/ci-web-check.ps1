$declarations = 'web/src/auto-imports.d.ts', 'web/src/components.d.ts'
$original = @{}
foreach ($file in $declarations) {
  $original[$file] = (Get-Content -Raw -Encoding UTF8 $file).Replace("`r`n", "`n")
}
$version = node -p "require('./npm/package.json').version"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$env:Path = "$env:WINDIR\System32;$env:Path"
node scripts/stage-web.mjs staging/release-web $version $env:GITHUB_SHA
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$notices = tar.exe -xOzf "staging/release-web/dimcode-web-$version.tar.gz" dimcode-web/web/THIRD-PARTY-NOTICES.txt
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
foreach ($package in 'vue', 'vue-router', 'pinia') {
  $entry = $notices -cmatch "^${package}@"
  if (-not $entry) { throw "Missing third-party notice for $package" }
  Write-Output $entry
}
foreach ($file in $declarations) {
  if ((Get-Content -Raw -Encoding UTF8 $file).Replace("`r`n", "`n") -cne $original[$file]) {
    throw "Generated declaration changed: $file"
  }
}
