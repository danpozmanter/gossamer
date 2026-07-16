package main

import "fmt"

func main() {
	total := int64(0)
	for round := int64(0); round < 2_000; round++ {
		values := make([]int64, 0, 1_000)
		for i := int64(0); i < 1_000; i++ {
			values = append(values, (i*17+round)%251)
		}
		total += values[123] + values[987] + int64(len(values))
	}
	fmt.Println(total)
}
