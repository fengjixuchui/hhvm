<?hh

<<__EntryPoint>>
function main(): void {
  $key = "\x26\xbd\xbd\xbd\xff\x60\xbf\xff\xff\x60";
  $salt1 = "\x24\x32\x78\x24\x31\x30\x24\x24\x35\x24\xad\x20\x20\x26\xff\x60\xbf\xff\xff\x60\x24\x31\x78\xa8\xa8\xa0\x01\x01\x01\x01\x01\x01";
  $salt2 = "\x24\x32\x78\x24\x31\x30\x24\x24\x35";

  var_dump(base64_encode(crypt($key, $salt1)));
  var_dump(base64_encode(crypt($key, $salt2)));
}
