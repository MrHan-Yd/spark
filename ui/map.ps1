Add-Type -AssemblyName System.Drawing
$bmp = [System.Drawing.Bitmap]::FromFile("D:\demo\test01\spark\ui\shot.png")
$w = $bmp.Width
$h = $bmp.Height
for ($y = 0; $y -lt $h; $y += 16) {
    $row = ""
    for ($x = 0; $x -lt $w; $x += 16) {
        $c = $bmp.GetPixel($x, $y)
        $lum = (0.299 * $c.R + 0.587 * $c.G + 0.114 * $c.B)
        if ($lum -gt 230) { $row += "W" }
        elseif ($lum -gt 170) { $row += "w" }
        elseif ($lum -gt 110) { $row += "." }
        elseif ($lum -gt 65) { $row += "," }
        elseif ($lum -gt 35) { $row += ":" }
        else { $row += " " }
    }
    Write-Host ("{0,3} {1}" -f $y, $row)
}
$bmp.Dispose()
