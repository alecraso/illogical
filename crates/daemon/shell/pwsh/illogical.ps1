# illogical shell integration for PowerShell (pwsh 7 and Windows PowerShell
# 5.1).
#
# illogicald starts PowerShell with `-NoExit -EncodedCommand` and this script
# (S29): Windows' default execution policy runs no script file (a profile,
# this one), but an inline command isn't covered. It runs after the user's
# profile and wraps whatever prompt that set up (oh-my-posh, Starship, ...).
#
# The hooks tell the terminal (OSC 133, plus VS Code's OSC 633 for the
# command line) where prompts and commands begin and end, the exit code, and
# the working directory (OSC 7). Nothing here changes how the shell behaves.

if (-not $global:__illogical_hooked) {
  $global:__illogical_hooked = $true
  $global:__illogical_first = $true
  $global:__illogical_prompt = $function:prompt
  # The window title, as other shells keep it (the directory at the prompt,
  # the command while it runs), unless the profile set one of its own.
  $global:__illogical_title = $Host.UI.RawUI.WindowTitle
  function global:__illogical_set_title([string]$t) {
    if ($Host.UI.RawUI.WindowTitle -ne $global:__illogical_title) { return }
    $Host.UI.RawUI.WindowTitle = $t
    $global:__illogical_title = $Host.UI.RawUI.WindowTitle
  }

  # The command line, escaped so it fits in an OSC: `\\` for a backslash,
  # `\xHH` for ';' and control characters.
  function global:__illogical_escape([string]$s) {
    $out = New-Object System.Text.StringBuilder
    foreach ($c in $s.ToCharArray()) {
      $n = [int]$c
      if ($c -eq '\') { [void]$out.Append('\\') }
      elseif ($c -eq ';') { [void]$out.Append('\x3b') }
      elseif ($n -lt 0x20 -or $n -eq 0x7f) { [void]$out.Append(('\x{0:x2}' -f $n)) }
      else { [void]$out.Append($c) }
    }
    $out.ToString()
  }

  # Before each prompt: the last command's exit code (a native program's
  # own, or 1 when a cmdlet failed), the directory, and "a prompt starts
  # here"; after it, "the input starts here". $LASTEXITCODE outlives the
  # native program that set it, so it counts only when it changed.
  $global:__illogical_lec = $global:LASTEXITCODE
  function global:prompt {
    $ok = $global:?
    $lec = $global:LASTEXITCODE
    $code = if ($ok) { 0 } elseif ($lec -and $lec -ne $global:__illogical_lec) { $lec } else { 1 }
    $global:__illogical_lec = $lec
    $e = [char]27; $bel = [char]7
    $s = ''
    if ($global:__illogical_first) { $global:__illogical_first = $false } else { $s += "$e]133;D;$code$bel" }
    $loc = $executionContext.SessionState.Path.CurrentLocation
    if ($loc.Provider.Name -eq 'FileSystem') {
      $path = ($loc.ProviderPath -replace '\\', '/') -replace '%', '%25' -replace ' ', '%20'
      $s += "$e]7;file://$env:COMPUTERNAME/$path$bel"
    }
    __illogical_set_title $loc.Path
    $s += "$e]133;A$bel"
    $s + (& $global:__illogical_prompt) + "$e]133;B$bel"
  }

  # When a line has been read: the command line, then "output starts here".
  # PSReadLine reads lines through this function; without it there are no
  # command lines, only the prompts.
  if (-not (Get-Module PSReadLine)) { Import-Module PSReadLine -ErrorAction SilentlyContinue }
  if (Test-Path Function:\PSConsoleHostReadLine) {
    $global:__illogical_readline = $function:PSConsoleHostReadLine
    function global:PSConsoleHostReadLine {
      $line = & $global:__illogical_readline
      if ($line.Trim()) { __illogical_set_title $line.Trim() }
      $e = [char]27; $bel = [char]7
      [Console]::Write("$e]633;E;$(__illogical_escape $line)$bel$e]133;C$bel")
      $line
    }
  }
}
