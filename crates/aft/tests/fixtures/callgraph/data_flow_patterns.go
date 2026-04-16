package callgraph

import "encoding/json"

type User struct {
	Name string
}

type Wrapper struct {
	Name string
}

type Ctx struct{}

// Gap 1: reference arg (&u) should produce a direct parameter hop.
func refArgCase(u User) {
	saveRef(&u)
}

func saveRef(user *User) {}

// Gap 2: field access as arg (u.Name) should produce an approximate parameter hop.
func fieldArgCase(u User) {
	consumeString(u.Name)
}

func consumeString(name string) {}

// Gap 3: struct literal wrap (Wrapper{Name: name}) should produce an approximate
// assignment hop, and subsequent uses of the new binding should be tracked.
func structLitCase(name string) {
	w := Wrapper{Name: name}
	saveWrapper(w)
}

func saveWrapper(wr Wrapper) {}

// Gap 4: intrinsic pointer-arg write (json.Unmarshal(raw, &user)) should bind
// raw's flow into user, so the subsequent call passes user as tracked.
func pointerWriteCase(raw []byte) {
	var user User
	_ = json.Unmarshal(raw, &user)
	consumeUser(user)
}

func consumeUser(u User) {}

// Gap 5: method receiver (u.saveMethod(...)) should produce a parameter hop
// into the method's receiver parameter.
func methodReceiverCase(u User) {
	u.saveMethod(Ctx{})
}

func (u *User) saveMethod(c Ctx) {}
