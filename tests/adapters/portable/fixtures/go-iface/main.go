package main

import (
	"example.com/shop/orders"
	"example.com/shop/store"
)

func main() {
	_ = orders.CreateOrder(&store.MemStore{}, "o-1")
}
