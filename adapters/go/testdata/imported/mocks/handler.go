package mocks

import "net/http"

type MockHandler struct{}

func (*MockHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {}
