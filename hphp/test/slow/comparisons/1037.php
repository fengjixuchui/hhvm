<?hh



<<__EntryPoint>>
function main_1037() {
$i = 0;
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=true) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=true) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = true;
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= true	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=false) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=false) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = false;
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= false	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=1) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=1) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = 1;
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= 1	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=0) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=0) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = 0;
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= 0	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=-1) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=-1) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = -1;
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= -1	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>='1') ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >='1') ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = '1';
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= '1'	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>='0') ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >='0') ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = '0';
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= '0'	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>='-1') ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >='-1') ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = '-1';
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= '-1'	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=null) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=null) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = null;
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= null	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= array()	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=varray[1]) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=varray[1]) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = varray[1];
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= array(1)	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=varray[2]) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=varray[2]) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = varray[2];
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= array(2)	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=varray['1']) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=varray['1']) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = varray['1'];
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= array('1')	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=darray['0' => '1']) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=darray['0' => '1']) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = darray['0' => '1'];
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= array('0' => '1')	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=varray['a']) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=varray['a']) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = varray['a'];
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= array('a')	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=darray['a' => 1]) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=darray['a' => 1]) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = darray['a' => 1];
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= array('a' => 1)	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=darray['b' => 1]) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=darray['b' => 1]) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = darray['b' => 1];
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= array('b' => 1)	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=darray['a' => 1, 'b' => 2]) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=darray['a' => 1, 'b' => 2]) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = darray['a' => 1, 'b' => 2];
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= array('a' => 1, 'b' => 2)	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=varray[darray['a' => 1]]) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=varray[darray['a' => 1]]) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = varray[darray['a' => 1]];
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= array(array('a' => 1))	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=varray[darray['b' => 1]]) ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >=varray[darray['b' => 1]]) ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = varray[darray['b' => 1]];
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= array(array('b' => 1))	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>='php') ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >='php') ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = 'php';
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= 'php'	";
 print "\n";
 print ++$i;
 print "\t";
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>='') ? 'Y' : 'N';
 $a = 1;
 $a = 't';
 $a = __hhvm_intrinsics\dummy_cast_to_kindofarray(vec[]);
 print ($a >='') ? 'Y' : 'N';
 $b = 1;
 $b = 't';
 $b = '';
 print (__hhvm_intrinsics\dummy_cast_to_kindofarray(vec[])>=$b) ? 'Y' : 'N';
 print ($a >=$b) ? 'Y' : 'N';
 print "\t";
 print "array() >= ''	";
 print "\n";
}
