// Background sync worker that reconciles local and remote notes.
package com.example.sync

data class Note(val id: String, val body: String)

object SyncWorker {
    suspend fun run(notes: List<Note>): Int {
        return notes.size
    }

    private fun <T> merge(a: List<T>, b: List<T>): List<T> = a + b
}

fun Note.isEmpty(): Boolean = body.isBlank()
