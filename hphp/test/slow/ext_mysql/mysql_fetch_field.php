<?hh
<<__EntryPoint>> function main(): void {
require_once('connect.inc');
list($host, $user, $passwd, $db) = connection_settings();
$conn = mysql_connect($host, $user, $passwd);
var_dump(create_test_table($db, 'fetch_field'));
var_dump(mysql_query(
  "insert into test_fetch_field (name) values ('test'),('test2')"));

$res = mysql_query('select * from test_fetch_field');
var_dump(mysql_fetch_field($res, 1)->name);
}
