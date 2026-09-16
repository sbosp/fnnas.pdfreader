package main

import (
	"net/http"
	"strings"
)

type User struct {
	UID      string
	Username string
	IsAdmin  bool
}

func safeName(uid string) string {
	var b strings.Builder
	for _, r := range uid {
		if (r >= '0' && r <= '9') || (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || r == '-' || r == '_' {
			b.WriteRune(r)
		}
	}
	if b.Len() == 0 {
		return "anon"
	}
	return b.String()
}

func userFrom(r *http.Request) *User {
	uid := r.Header.Get("X-Trim-Userid")
	isAdmin := r.Header.Get("X-Trim-Isadmin") == "true"
	username := r.Header.Get("X-Trim-Username")
	if cfg.RequireAuth && uid == "" {
		return nil
	}
	if uid == "" {
		uid = "debug"
	}
	if username == "" {
		username = uid
	}
	return &User{UID: uid, Username: username, IsAdmin: isAdmin}
}

func requireUser(next func(http.ResponseWriter, *http.Request, *User)) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		u := userFrom(r)
		if u == nil {
			http.Error(w, "Forbidden: gateway auth required", http.StatusForbidden)
			return
		}
		next(w, r, u)
	}
}
