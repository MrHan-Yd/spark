Add-Type -AssemblyName System.Drawing
$src = [System.Drawing.Bitmap]::FromFile("D:\demo\test01\spark\ui\shot.png")
# 左上角 60x60 放大 8 倍
$crop = New-Object System.Drawing.Bitmap 60, 60
$g = [System.Drawing.Graphics]::FromImage($crop)
$g.DrawImage($src, (New-Object System.Drawing.Rectangle 0,0,60,60), (New-Object System.Drawing.Rectangle 0,0,60,60), [System.Drawing.GraphicsUnit]::Pixel)
$g.Dispose()
$big = New-Object System.Drawing.Bitmap 480, 480
$g2 = [System.Drawing.Graphics]::FromImage($big)
$g2.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
$g2.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
$g2.DrawImage($crop, (New-Object System.Drawing.Rectangle 0,0,480,480), (New-Object System.Drawing.Rectangle 0,0,60,60), [System.Drawing.GraphicsUnit]::Pixel)
$g2.Dispose()
$crop.Dispose()
$big.Save("D:\demo\test01\spark\ui\corner_big.png", [System.Drawing.Imaging.ImageFormat]::Png)
$big.Dispose()
$src.Dispose()
Write-Host "saved corner_big.png"
