// Package server wires HTTP handlers to the session store.
package server

import "net/http"

type Server struct {
	mux *http.ServeMux
}

type Store interface {
	Get(id string) string
}

func New() *Server { return &Server{mux: http.NewServeMux()} }

func (s *Server) handleLogin(w http.ResponseWriter, r *http.Request) {
	if r.Method != "POST" {
		return
	}
}
