<?hh
<<__EntryPoint>> function main(): void {
//
// This script will generate a Seg Fault on Linux
//
$im  = imagecreatetruecolor(300, 300);
$w  = imagecolorallocate($im, 255, 255, 255);
$red = imagecolorallocate($im, 255, 0, 0);

imagefilledrectangle($im,0,0,299,299,$w);

imageantialias($im,true);
imageline($im, 299, 299, 0, 299, $red);

imagedestroy($im);

echo "Alive\n";
}
