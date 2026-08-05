Add-Type -AssemblyName System.Drawing
$bmp = [System.Drawing.Bitmap]::FromFile("D:\demo\test01\spark\ui\shot.png")
$w = $bmp.Width
$h = $bmp.Height
Write-Host "size: ${w}x${h}"
function Px($x, $y) {
    $c = $bmp.GetPixel($x, $y)
    return "#{0:X2}{1:X2}{2:X2}" -f $c.R, $c.G, $c.B
}
# 顶边采样
foreach ($y in 0,1,2,3,4) {
    $s = @()
    foreach ($x in 0,2,4,6,8,10,12,16,24,40,80,160,320,640,700,740,760,770,776,779) {
        if ($x -lt $w) { $s += "x$x=" + (Px $x $y) }
    }
    Write-Host "top y=$y : $($s -join ' ')"
}
# 左边采样
foreach ($y in 0,5,10,20,40,100,200,300,400,500,540,556) {
    $s = @()
    foreach ($x in 0,1,2,3,4,6,8,10) {
        if ($y -lt $h) { $s += "x$x=" + (Px $x $y) }
    }
    Write-Host "left y=$y : $($s -join ' ')"
}
# 对角线：从 (0,0) 向内找非白像素边界，估计圆角半径
foreach ($y in 0,1,2,3,4,5,6,7,8,9,10,12,14,16,18,20,24,28) {
    Write-Host ("diag({0},{0})=" -f $y) + (Px $y $y)
}
# 底部采样
foreach ($y in $h-1, $h-2, $h-3, $h-4) {
    $s = @()
    foreach ($x in 0,2,4,6,8,10,16,40,160,320,640,700,740,770,776,779) {
        if ($x -lt $w) { $s += "x$x=" + (Px $x $y) }
    }
    Write-Host "bottom y=$y : $($s -join ' ')"
}
$bmp.Dispose()
