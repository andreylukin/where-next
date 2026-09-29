#!/usr/bin/env python3
# Copyright 2024 Example Authors. Licensed under the Apache License.
"""Token authentication helpers for the request pipeline."""

import hmac


class TokenStore:
    def __init__(self, secret):
        self.secret = secret

    async def verify(self, token):
        return hmac.compare_digest(token, self.secret)


def authenticate(request):
    return request.headers.get("Authorization")
