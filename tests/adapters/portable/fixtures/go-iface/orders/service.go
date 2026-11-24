package orders

import "example.com/shop/store"

// CreateOrder saves an order through the Saver interface.
func CreateOrder(s store.Saver, id string) error {
	return s.Save(id)
}
