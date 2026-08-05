Add-Type -AssemblyName System.Drawing
$bmp = [System.Drawing.Bitmap]::FromFile("D:\demo\test01\spark\ui\shot.png")
$w = $bmp.Width
$h = $bmp.Height
# ASCII 亮度图：左上角 60x40
for ($y = 0; $y -lt 40; $y++) {
    $row = ""
    for ($x = 0; $x -lt 60; $x++) {
        $c = $bmp.GetPixel($x, $y)
        $lum = (0.299 * $c.R + 0.587 * $c.G + 0.114 * $c.B)
        if ($lum -gt 240) { $row += "W" }          # 近白
        elseif ($lum -gt 180) { $row += "w" }      # 浅
        elseif ($lum -gt 120) { $row += "." }      # 中灰
        elseif ($lum -gt 70) { $row += "," }       # 深灰
        elseif ($lum -gt 35) { $row += ":" }       # 更深
        else { $row += " " }                       # 近黑
    }
    Write-Host ("{0,2} {1}" -f $y, $row)
}
$bmp.Dispose()
