# Build OLD/NEW/BAD and lay out the fake release for selfupdate.ps1.
# Run in C:\s29\arugula with the branch checked out.
param([string]$Old = "0.23.98", [string]$New = "0.23.99", [string]$Bad = "0.23.100",
      [string]$Su = "C:\s29\su", [string]$Profile = "release")
$ErrorActionPreference = "Stop"
$env:CARGO_INCREMENTAL = "0"
$repo = "C:\s29\arugula"
# conpty.dll + OpenConsole.exe: the installed desktop app's copies (the release's).
$extras = "$env:LOCALAPPDATA\arugula"
Set-Location $repo
Copy-Item Cargo.toml "$Su\Cargo.toml.orig"; Copy-Item Cargo.lock "$Su\Cargo.lock.orig"
$toml = Get-Content Cargo.toml -Raw
try {
  foreach ($v in $Old, $New, $Bad) {
    $t = [regex]::Replace($toml, '(?ms)(\[workspace\.package\]\s*\r?\nversion = )"[^"]*"', "`$1`"$v`"")
    [IO.File]::WriteAllText("$repo\Cargo.toml", $t)
    $flag = if ($Profile -eq "release") { "--release" } else { $null }
    cargo build $flag -p arugulad -p arugula
    if ($LASTEXITCODE) { throw "build $v failed" }
    $d = "$Su\bin\$v"; New-Item -ItemType Directory -Force $d | Out-Null
    Copy-Item "target\$Profile\arugulad.exe", "target\$Profile\arugula.exe" $d
    Copy-Item "$extras\conpty.dll", "$extras\OpenConsole.exe" $d
    "$v -> $(& "$d\arugulad.exe" --version)"
  }
} finally {
  Copy-Item "$Su\Cargo.toml.orig" Cargo.toml -Force; Copy-Item "$Su\Cargo.lock.orig" Cargo.lock -Force
}

# The release: arugula-V-x86_64-pc-windows-msvc.zip holding the folder, and SHA256SUMS.
foreach ($v in $New, $Bad) {
  $stem = "arugula-$v-x86_64-pc-windows-msvc"
  $dl = "$Su\rel\releases\download\v$v"; New-Item -ItemType Directory -Force $dl | Out-Null
  $stage = "$Su\stage\$stem"; Remove-Item -Recurse -Force "$Su\stage" -ea 0
  New-Item -ItemType Directory -Force $stage | Out-Null
  Copy-Item "$Su\bin\$v\*" $stage
  Compress-Archive -Path $stage -DestinationPath "$dl\$stem.zip" -Force
  $sum = (Get-FileHash "$dl\$stem.zip" -Algorithm SHA256).Hash.ToLower()
  if ($v -eq $Bad) { $sum = "0" * 64 }   # BAD's sum is wrong
  [IO.File]::WriteAllText("$dl\SHA256SUMS", "$sum  $stem.zip`n")
}
Remove-Item -Recurse -Force "$Su\stage"
Set-Content "$Su\rel\latest.txt" $New
Get-ChildItem -Recurse "$Su\rel" | Select-Object FullName, Length | Format-Table -AutoSize
