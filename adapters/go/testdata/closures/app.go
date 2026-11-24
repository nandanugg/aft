package app

func Work()  {}
func Other() {}

func Closures(items []int) {
	run := func() { Work() }
	run()
	go func() { Work() }()
	defer func() { Other() }()
	for range items {
		func() { Work() }()
	}
	f := Other
	f()
}

// Unicode: the column counts bytes, not characters.
func Unicode() { _ = "héllo, 世界"; Work() }
