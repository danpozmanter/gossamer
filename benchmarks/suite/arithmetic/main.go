package main

import "fmt"

func main() {
	var acc int64 = 17
	for i := int64(0); i < 5_000_000; i++ {
		acc = (acc*31 + i*7) % 1_000_003
	}
	fmt.Println(acc)
}
