$targetDir = [System.IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)
$stampFile = Join-Path $targetDir '.cache-stamp'
$maxBytes = 12GB
$reset = $false
if (Test-Path $targetDir) {
  $current = if (Test-Path $stampFile) { (Get-Content -Raw $stampFile).Trim() } else { '' }
  if ($current -ne $env:CACHE_STAMP) {
    Write-Output 'Resetting Windows target cache (toolchain or profile stamp changed).'
    $reset = $true
  } else {
    $bytes = (Get-ChildItem -LiteralPath $targetDir -Recurse -File -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
    if ($bytes -gt $maxBytes) {
      Write-Output "Resetting Windows target cache ($([math]::Round($bytes / 1GB, 2)) GiB exceeded 12 GiB budget)."
      $reset = $true
    }
  }
}
if ($reset) {
  Remove-Item -LiteralPath $targetDir -Recurse -Force
}
New-Item -ItemType Directory -Path $targetDir -Force | Out-Null
Set-Content -LiteralPath $stampFile -Value $env:CACHE_STAMP -NoNewline
"CARGO_TARGET_DIR=$targetDir" >> $env:GITHUB_ENV
