package main

import "fmt"

func worker(id int64, ch chan<- int64) {
	var total int64
	for i := int64(0); i < 100_000; i++ {
		total += (i + id) % 97
	}
	ch <- total
}
func main() {
	ch := make(chan int64, 8)
	for id := int64(0); id < 8; id++ {
		go worker(id, ch)
	}
	var total int64
	for i := 0; i < 8; i++ {
		total += <-ch
	}
	fmt.Println(total)
}
