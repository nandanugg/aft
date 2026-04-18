package dispatch

import "net/http"

// HTTP-style handler registration

func handleHome(w http.ResponseWriter, r *http.Request) {}
func handleAPI(w http.ResponseWriter, r *http.Request)  {}

func RegisterHTTPHandlers(mux *http.ServeMux) {
	mux.HandleFunc("/", handleHome)
	mux.HandleFunc("/api", handleAPI)
}
