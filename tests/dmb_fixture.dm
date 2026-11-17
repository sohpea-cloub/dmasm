// Compiled to dmb_fixture.dmb by DreamMaker 516 for tests/dmb.rs: a type with a var
// and a proc, and global procs with a loop, a call and a `new`.

/datum/fixture_thing
	var/value = 0

/datum/fixture_thing/proc/scale(mult)
	return src.value * mult

/proc/fixture_sum_to(n)
	var/total = 0
	var/i = 1
	while(i <= n)
		total += i
		i++
	return total

/proc/fixture_make_thing()
	var/datum/fixture_thing/thing = new /datum/fixture_thing()
	thing.value = 3.5
	return thing.scale(2)
