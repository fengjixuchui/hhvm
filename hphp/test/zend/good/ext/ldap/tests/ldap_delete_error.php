<?hh
<<__EntryPoint>> function main(): void {
require "connect.inc";
$link = ldap_connect_and_bind(test_host(), test_port(), test_user(), test_passwd(), test_protocol_version());
$base = test_base();
// Too few parameters
var_dump(ldap_delete());
var_dump(ldap_delete($link));

// Too many parameters
var_dump(ldap_delete($link, "$base", "Additional data"));

// Invalid DN
var_dump(
    ldap_delete($link, "weirdAttribute=val"),
    ldap_error($link),
    ldap_errno($link)
);

// Deleting unexisting data
var_dump(
    ldap_delete($link, "dc=my-domain,$base"),
    ldap_error($link),
    ldap_errno($link)
);
echo "===DONE===\n";
}
