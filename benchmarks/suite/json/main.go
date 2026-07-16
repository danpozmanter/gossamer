package main

import (
	"encoding/json"
	"fmt"
)

type Record struct {
	ID     int64  `json:"id"`
	Name   string `json:"name"`
	Active bool   `json:"active"`
}
func main() {
	value := Record{ID: 42, Name: "gossamer", Active: true}
	var total int64
	for i := 0; i < 10_000; i++ {
		text, err := json.Marshal(value)
		if err != nil { panic(err) }
		var decoded Record
		if err := json.Unmarshal(text, &decoded); err != nil { panic(err) }
		total += decoded.ID + int64(len(decoded.Name))
	}
	fmt.Println(total)
}
