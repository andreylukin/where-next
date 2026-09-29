/*
 * Copyright (c) 2023 Example. All rights reserved.
 */
package com.example.net;

/** Retries uploads when the storage backend times out. */
public class Retry {
    private final int limit;

    public Retry(int limit) {
        this.limit = limit;
    }

    public static boolean shouldRetry(int attempt, Exception e) {
        return attempt < 3;
    }

    private void backoff(long millis) throws InterruptedException {
        Thread.sleep(millis);
    }
}

interface Policy {
    boolean allow(int attempt);
}
