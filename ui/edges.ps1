Add-Type -AssemblyName System.Drawing
$bmp = [System.Drawing.Bitmap]::FromFile("D:\demo\test01\spark\ui\shot.png")
$w = $bmp.Width
$h = $bmp.Height
function EdgeWhite($y1, $y2, $x1, $x2) {
    $white = 0
    $total = 0
    for ($y = $y1; $y -le $y2; $y++) {
        for ($x = $x1; $x -le $x2; $x++) {
            $c = $bmp.GetPixel($x, $y)
            $lum = (0.299 * $c.R + 0.587 * $c.G + 0.114 * $c.B)
            if ($lum -gt 200) { $white++ }
            $total++
        }
    }
    return ("{0}/{1}" -f $white, $total)
}
Write-Host ("top y0-4   : " + (EdgeWhite 0 4 20 ($w - 21)))
Write-Host ("top y0-1   : " + (EdgeWhite 0 1 20 ($w - 21)))
Write-Host ("top y1-3   : " + (EdgeWhite 1 3 20 ($w - 21)))
Write-Host ("bottom y-5..-1: " + (EdgeWhite ($h - 5) ($h - 1) 20 ($w - 21)))
Write-Host ("bottom y-4..-1: " + (EdgeWhite ($h - 4) ($h - 1) 20 ($w - 21)))
Write-Host ("left x0-4   : " + (EdgeWhite 20 ($h - 21) 0 4))
Write-Host ("left x0-1   : " + (EdgeWhite 20 ($h - 21) 0 1))
Write-Host ("left x1-3   : " + (EdgeWhite 20 ($h - 21) 1 3))
Write-Host ("right x-5..-1: " + (EdgeWhite 20 ($h - 21) ($w - 5) ($w - 1)))
Write-Host ("right x-4..-1: " + (EdgeWhite 20 ($h - 21) ($w - 4) ($w - 1)))
foreach ($x in 60, 100, 200, 400, 600, 680) {
    $c = $bmp.GetPixel($x, 40)
    Write-Host ("row40 x={0} #{1:X2}{2:X2}{3:X2}" -f $x, $c.R, $c.G, $c.B)
}
$bmp.Dispose()
