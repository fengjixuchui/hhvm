<?hh
<<__EntryPoint>> function main(): void {
require "connect.inc";
$link = ldap_connect_and_bind(test_host(), test_port(), test_user(), test_passwd(), test_protocol_version());
$base = test_base();
insert_dummy_data($link, $base);
$mods = array(
    array(
        "attrib"    => "telephoneNumber",
        "modtype"   => LDAP_MODIFY_BATCH_ADD,
        "values"    => array(
            "+1 555 5551717"
        )
    ),
    array(
        "attrib"    => "sn",
        "modtype"   => LDAP_MODIFY_BATCH_REPLACE,
        "values"    => array("Brown-Smith")
    ),
    array(
        "attrib"    => "description",
        "modtype"   => LDAP_MODIFY_BATCH_REMOVE_ALL
    )
);

var_dump(
    ldap_modify_batch($link, "cn=userA,$base", $mods),
    ldap_get_entries($link, ldap_search($link, "$base", "(sn=Brown-Smith)"))
);
echo "===DONE===\n";
//--remove_dummy_data($link, $base);
}
