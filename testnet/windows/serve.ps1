# A fake GitHub release on loopback, for selfupdate.ps1.
#   GET /releases/latest       -> 302 to /releases/tag/v<contents of latest.txt>
#   GET /releases/download/... -> the file under $Root\releases\download\...
# A raw TcpListener, not HttpListener: no URL ACL (admin) needed.
param([string]$Root = "C:\s29\su\rel", [int]$Port = 8791)
$ErrorActionPreference = "Stop"
$l = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, $Port)
$l.Start()
"serving $Root on 127.0.0.1:$Port" | Out-File -Append "$Root\serve.log"
while ($true) {
  $c = $l.AcceptTcpClient()
  try {
    $s = $c.GetStream()
    $r = [IO.StreamReader]::new($s, [Text.Encoding]::ASCII, $false, 4096, $true)
    $line = $r.ReadLine()
    while (($h = $r.ReadLine()) -ne $null -and $h -ne "") { }
    $path = ($line -split " ")[1]
    $body = [byte[]]@()
    if ($path -eq "/releases/latest") {
      $v = (Get-Content "$Root\latest.txt" -Raw).Trim()
      $head = "HTTP/1.1 302 Found`r`nLocation: http://127.0.0.1:$Port/releases/tag/v$v`r`n"
    } else {
      $f = Join-Path $Root ($path.TrimStart("/") -replace "/", "\")
      if ($path -notmatch "\.\." -and (Test-Path -LiteralPath $f -PathType Leaf)) {
        $body = [IO.File]::ReadAllBytes($f)
        $head = "HTTP/1.1 200 OK`r`nContent-Type: application/octet-stream`r`n"
      } else {
        $head = "HTTP/1.1 404 Not Found`r`n"
      }
    }
    "$(Get-Date -f HH:mm:ss) $line -> $(($head -split "`r`n")[0])" | Out-File -Append "$Root\serve.log"
    $out = [Text.Encoding]::ASCII.GetBytes("$head" + "Content-Length: $($body.Length)`r`nConnection: close`r`n`r`n")
    $s.Write($out, 0, $out.Length)
    if ($body.Length) { $s.Write($body, 0, $body.Length) }
    $s.Flush()
  } catch {
    "error: $_" | Out-File -Append "$Root\serve.log"
  } finally { $c.Close() }
}
