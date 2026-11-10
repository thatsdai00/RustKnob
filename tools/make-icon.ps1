# Draws the app icon (dark rounded square, white upload arrow over a tray line) into assets\rustknob.ico.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
Add-Type -AssemblyName System.Drawing

function Draw([int]$n) {
    $bmp = New-Object System.Drawing.Bitmap $n, $n
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)
    $r = $n * 0.22
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $path.AddArc(0, 0, $d, $d, 180, 90); $path.AddArc($n - $d - 1, 0, $d, $d, 270, 90)
    $path.AddArc($n - $d - 1, $n - $d - 1, $d, $d, 0, 90); $path.AddArc(0, $n - $d - 1, $d, $d, 90, 90)
    $path.CloseFigure()
    $g.FillPath((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 18, 18, 22))), $path)
    $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::White), ($n * 0.085)
    $pen.StartCap = 'Round'; $pen.EndCap = 'Round'; $pen.LineJoin = 'Round'
    $cx = $n / 2
    $g.DrawLine($pen, $cx, $n * 0.64, $cx, $n * 0.24)                                  # shaft
    $g.DrawLines($pen, @([System.Drawing.PointF]::new($n * 0.30, $n * 0.42), [System.Drawing.PointF]::new($cx, $n * 0.24), [System.Drawing.PointF]::new($n * 0.70, $n * 0.42)))  # head
    $g.DrawLines($pen, @([System.Drawing.PointF]::new($n * 0.24, $n * 0.62), [System.Drawing.PointF]::new($n * 0.24, $n * 0.77), [System.Drawing.PointF]::new($n * 0.76, $n * 0.77), [System.Drawing.PointF]::new($n * 0.76, $n * 0.62)))  # tray
    $g.Dispose()
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    , $ms.ToArray()
}

$sizes = 16, 24, 32, 48, 64, 256
$pngs = $sizes | ForEach-Object { , (Draw $_) }
$out = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter $out
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]; $b = if ($s -ge 256) { 0 } else { $s }
    $w.Write([byte]$b); $w.Write([byte]$b); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]$pngs[$i].Length); $w.Write([uint32]$offset)
    $offset += $pngs[$i].Length
}
foreach ($p in $pngs) { $w.Write($p) }
[IO.File]::WriteAllBytes((Join-Path (Get-Location) 'assets\rustknob.ico'), $out.ToArray())
[IO.File]::WriteAllBytes((Join-Path (Get-Location) 'build\icon-preview.png'), $pngs[5])
