# Install Arugula on Windows: download a release, check it, and run
# `arugulad install`, which puts arugulad and arugula in
# %LOCALAPPDATA%\Programs\arugula (on your PATH) and starts the daemon at
# logon as a scheduled task. Run it again to upgrade; the daemon's flags and
# your panes are kept.
#
#   irm https://arugula.io/install.ps1 | iex
#
# $env:ARUGULA_VERSION = 'vX.Y.Z'   a release tag (default: the latest)
# $env:ARUGULA_NO_START = '1'        register the task without starting it
# $env:ARUGULA_DOWNLOAD_URL = '...'  where the release's files are, instead
#                                      of GitHub (a mirror, or a local folder)
# Each can also be ILLOGICAL_..., its name before the rename.
#
# On a machine with illogical, `arugulad install` moves it over: it replaces
# the illogicald task and keeps illogical.exe and illogicald.exe callable.

# A block of its own: `iex` runs this in your shell, which an `exit` would end.
& {
  $ErrorActionPreference = 'Stop'
  $ProgressPreference = 'SilentlyContinue'
  $repo = 'https://github.com/arugula-salad/arugula'
  # ARUGULA_X, or ILLOGICAL_X, its name before the rename, for scripts that
  # set that (#505, drop in #508). Read, not set: this runs in your shell.
  $setting = {
    param($n)
    $new = [Environment]::GetEnvironmentVariable("ARUGULA_$n")
    if ($null -ne $new) { $new } else { [Environment]::GetEnvironmentVariable("ILLOGICAL_$n") }
  }

  # x64 builds; Windows on Arm runs them too.
  if (-not [Environment]::Is64BitOperatingSystem) { throw 'arugula: there is no 32-bit Windows release' }
  $target = 'x86_64-pc-windows-msvc'

  $version = & $setting 'VERSION'
  if (-not $version) {
    # releases/latest redirects to releases/tag/<tag>. Not GitHub's API: its
    # unauthenticated limit is shared by everyone behind an IP.
    $req = [System.Net.HttpWebRequest]::Create("$repo/releases/latest")
    $req.AllowAutoRedirect = $false
    $res = $req.GetResponse()
    $version = ($res.Headers['Location'] -split '/releases/tag/')[-1]
    $res.Close()
    if ($version -notmatch '^v[0-9]') { throw "arugula: couldn't find the latest release at $repo/releases" }
  }

  $name = "arugula-$($version.TrimStart('v'))-$target"
  $base = if (& $setting 'DOWNLOAD_URL') { & $setting 'DOWNLOAD_URL' } else { "$repo/releases/download/$version" }
  $tmp = Join-Path ([IO.Path]::GetTempPath()) "arugula-install-$PID"
  New-Item -ItemType Directory -Force $tmp | Out-Null
  try {
    "downloading $name"
    $get = { param($file) if (Test-Path $base) { Copy-Item (Join-Path $base $file) $tmp } else { Invoke-WebRequest "$base/$file" -OutFile (Join-Path $tmp $file) -UseBasicParsing } }
    & $get "$name.zip"
    & $get 'SHA256SUMS'
    $line = Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { $_ -match " $([regex]::Escape("$name.zip"))$" } | Select-Object -First 1
    if (-not $line) { throw "arugula: $name.zip isn't in SHA256SUMS" }
    $want = ($line -split '\s+')[0].ToLower()
    $got = (Get-FileHash (Join-Path $tmp "$name.zip") -Algorithm SHA256).Hash.ToLower()
    if ($want -ne $got) { throw "arugula: checksum mismatch for $name.zip" }

    Expand-Archive (Join-Path $tmp "$name.zip") $tmp -Force
    $daemon = Join-Path $tmp "$name\arugulad.exe"
    if (& $setting 'NO_START') { & $daemon install --no-start } else { & $daemon install }
    if ($LASTEXITCODE -ne 0) { throw "arugula: arugulad install failed ($LASTEXITCODE)" }

    $bin = Join-Path $env:LOCALAPPDATA 'Programs\arugula'
    ''
    'Arugula is installed. In a new terminal:'
    '  arugula web       open it in your browser, signed in'
    '  arugula run pwsh  a pane, from the command line'
    "(Or now: & '$bin\arugula.exe' web)"
  } finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
  }
}
