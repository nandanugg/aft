package app

import "net/http"

type handler struct{}

func (handler) ServeHTTP(w http.ResponseWriter, r *http.Request) {}

func Serve(h http.Handler, err error, w http.ResponseWriter, r *http.Request) {
	h.ServeHTTP(w, r)
	_ = err.Error()
}
