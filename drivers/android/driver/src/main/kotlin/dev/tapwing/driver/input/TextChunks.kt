// Copyright (c) 2022 mobile.dev inc.
// Modifications copyright (c) 2026 the Tapwing authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
// Ported from Maestro 2.12.1 (github.com/mobile-dev-inc/maestro, tag cli-2.12.1),
// maestro-client/src/main/java/maestro/drivers/AndroidDriver.kt
// (`chunkPreservingSurrogatePairs`, `MAX_UNICODE_INPUT_CHUNK_SIZE`).
// Changes: moved from the host to the device.
package dev.tapwing.driver.input

object TextChunks {

    /** Maestro commits Unicode text through its IME in chunks of this many UTF-16 units. */
    const val MAX_CHUNK = 1000

    /**
     * Splits [text] into chunks of at most [maxChunkSize] UTF-16 code units
     * without severing a surrogate pair across a boundary.
     */
    fun split(text: String, maxChunkSize: Int = MAX_CHUNK): List<String> {
        require(maxChunkSize > 0) { "maxChunkSize must be positive, was $maxChunkSize" }
        val chunks = mutableListOf<String>()
        var start = 0
        while (start < text.length) {
            var end = minOf(start + maxChunkSize, text.length)
            if (end < text.length && Character.isHighSurrogate(text[end - 1]) && Character.isLowSurrogate(text[end])) {
                // Keep the pair together. With a chunk size of 1 there is no room to back off
                // (an empty chunk would never advance), so the whole pair goes into one chunk.
                end = if (end - 1 > start) end - 1 else end + 1
            }
            chunks.add(text.substring(start, end))
            start = end
        }
        return chunks
    }
}
