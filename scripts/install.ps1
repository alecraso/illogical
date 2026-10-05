# Install illogical on Windows: download a release, check it, and run
# `illogicald install`, which puts illogicald and illogical in
# %LOCALAPPDATA%\Programs\illogical (on your PATH) and starts the daemon at
# logon as a scheduled task. Run it again to upgrade; the daemon's flags and
# your panes are kept.
#
#   irm https://illogical.widgets.wtf/install.ps1 | iex
#
# $env:ILLOGICAL_VERSION = 'vX.Y.Z'   a release tag (default: the latest)
# $env:ILLOGICAL_NO_START = '1'        register the task without starting it
# $env:ILLOGICAL_DOWNLOAD_URL = '...'  where the release's files are, instead
#                                      of GitHub (a mirror, or a local folder)

# A block of its own: `iex` runs this in your shell, which an `exit` would end.
& {
  $ErrorActionPreference = 'Stop'
  $ProgressPreference = 'SilentlyContinue'
  $repo = 'https://github.com/arugula-salad/illogical'

  # x64 builds; Windows on Arm runs them too.
  if (-not [Environment]::Is64BitOperatingSystem) { throw 'illogical: there is no 32-bit Windows release' }
  $target = 'x86_64-pc-windows-msvc'

  $version = $env:ILLOGICAL_VERSION
  if (-not $version) {
    # releases/latest redirects to releases/tag/<tag>. Not GitHub's API: its
    # unauthenticated limit is shared by everyone behind an IP.
    $req = [System.Net.HttpWebRequest]::Create("$repo/releases/latest")
    $req.AllowAutoRedirect = $false
    $res = $req.GetResponse()
    $version = ($res.Headers['Location'] -split '/releases/tag/')[-1]
    $res.Close()
    if ($version -notmatch '^v[0-9]') { throw "illogical: couldn't find the latest release at $repo/releases" }
  }

  $name = "illogical-$($version.TrimStart('v'))-$target"
  $base = if ($env:ILLOGICAL_DOWNLOAD_URL) { $env:ILLOGICAL_DOWNLOAD_URL } else { "$repo/releases/download/$version" }
  $tmp = Join-Path ([IO.Path]::GetTempPath()) "illogical-install-$PID"
  New-Item -ItemType Directory -Force $tmp | Out-Null
  try {
    "downloading $name"
    $get = { param($file) if (Test-Path $base) { Copy-Item (Join-Path $base $file) $tmp } else { Invoke-WebRequest "$base/$file" -OutFile (Join-Path $tmp $file) -UseBasicParsing } }
    & $get "$name.zip"
    & $get 'SHA256SUMS'
    $line = Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { $_ -match " $([regex]::Escape("$name.zip"))$" } | Select-Object -First 1
    if (-not $line) { throw "illogical: $name.zip isn't in SHA256SUMS" }
    $want = ($line -split '\s+')[0].ToLower()
    $got = (Get-FileHash (Join-Path $tmp "$name.zip") -Algorithm SHA256).Hash.ToLower()
    if ($want -ne $got) { throw "illogical: checksum mismatch for $name.zip" }

    Expand-Archive (Join-Path $tmp "$name.zip") $tmp -Force
    $daemon = Join-Path $tmp "$name\illogicald.exe"
    if ($env:ILLOGICAL_NO_START) { & $daemon install --no-start } else { & $daemon install }
    if ($LASTEXITCODE -ne 0) { throw "illogical: illogicald install failed ($LASTEXITCODE)" }

    $bin = Join-Path $env:LOCALAPPDATA 'Programs\illogical'
    ''
    'illogical is installed. In a new terminal:'
    '  illogical web       open it in your browser, signed in'
    '  illogical run pwsh  a pane, from the command line'
    "(Or now: & '$bin\illogical.exe' web)"
  } finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
  }
}
