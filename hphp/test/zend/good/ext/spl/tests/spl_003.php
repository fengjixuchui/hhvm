<?hh
class a{}
class b extends a{}
class c extends b{}
class d{}
var_dump(class_parents(new c),
         class_parents("c"),
         class_parents(new b),
         class_parents("b"),
         class_parents("d"),
         class_parents("foo", false),
         class_parents("foo", true)
);

interface iface1{}
interface iface2{}
class f implements iface1, iface2{}
var_dump(class_implements(new a),
         class_implements("a"),
         class_implements("aaa"),
         class_implements("bbb", false)
);

echo "===DONE===\n";
