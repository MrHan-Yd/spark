Add-Type -AssemblyName System.Drawing
$bmp = [System.Drawing.Bitmap]::FromFile("D:\demo\test01\spark\ui\shot.png")
$w = $bmp.Width
$h = $bmp.Height
# 整图下采样：8px 一块取平均亮度，宽 100 列
$cols = 100
$stepX = [Math]::Ceiling($w / $cols)
$rows = [Math]::Ceiling($h / 8)
for ($ry = 0; $ry -lt $rows; $ry++) {
    $row = ""
    for ($rx = 0; $rx -lt $cols; $rx++) {
        $sum = 0.0
        $n = 0
        for ($dy = 0; $dy -lt 8; $dy++) {
            $y = $ry * 8 + $dy
            if ($y -ge $h) { continue }
            for ($dx = 0; $dx -lt $stepX; $dx++) {
                $x = $rx * $stepX + $dx
                if ($x -ge $w) { continue }
                $c = $bmp.GetPixel($x, $y)
                $sum += (0.299 * $c.R + 0.587 * $c.G + 0.114 * $c.B)
                $n++
            }
        }
        $lum = $sum / $n
        if ($lum -gt 230) { $row += "W" }
        elseif ($lum -gt 170) { $row += "w" }
        elseif ($lum -gt 110) { $row += "." }
        elseif ($lum -gt 65) { $row += "," }
        elseif ($lum -gt 35) { $row += ":" }
        else { $row += " " }
    }
    Write-Host ("{0,3} {1}" -f ($ry * 8), $row)
}
$bmp.Dispose()
