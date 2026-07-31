package com.reproit.android

import java.io.File

/**
 * On-disk spool for crash capsules, drained on the next process start.
 *
 * The crash path used to POST the capture batch synchronously on the crashing
 * thread. That races the OS: measured on a Pixel_9a, Android kills a crashed
 * process 168 to 768 ms after the fatal exception, while the first HTTP POST on
 * a cold process took 40 to 316 ms against a LOCALHOST ingest. The ranges
 * overlap, so delivery was intermittent (4 of 6 confirmed crashes), and with a
 * realistic 2 s ingest latency it dropped to 0 of 2. A capsule is the artifact
 * hermetic replay depends on, so losing it to a network race is not acceptable.
 *
 * The fix is the standard crash-reporter shape: on the crash path do only a
 * bounded LOCAL write (a few milliseconds, no network), then upload on the next
 * launch. Android's uncaught-exception handler differs from a POSIX signal
 * handler in that it may allocate and take locks, so the JSON is serialized
 * here rather than pre-staged; the only constraint is time, and a file write
 * beats a network round trip by orders of magnitude.
 *
 * Bounds, because an unbounded spool on a user's device is a defect of its own:
 * at most [MAX_FILES] capsules and [MAX_TOTAL_BYTES] on disk, oldest dropped
 * first, and a single capsule larger than [MAX_FILE_BYTES] is refused outright
 * rather than truncated into something that would replay as a different
 * failure. Delivery is at-most-once: a spooled file is deleted only after the
 * upload is accepted, and each file is claimed by rename before upload so two
 * drains cannot post it twice.
 */
internal class CapsuleSpool(private val directory: File) {
  companion object {
    /** Capsules retained on disk. Oldest is dropped when the cap is reached. */
    const val MAX_FILES = 8

    /** Total spool footprint. Oldest are dropped until the write fits. */
    const val MAX_TOTAL_BYTES = 1L * 1024 * 1024

    /** A single capsule larger than this is refused, never truncated. */
    const val MAX_FILE_BYTES = 512L * 1024

    private const val SUFFIX = ".capsule.json"
    private const val CLAIM_SUFFIX = ".uploading.json"
  }

  /**
   * Write one capsule to the spool. Called ON THE CRASH PATH, so it does the
   * minimum: one temp write plus an atomic rename. Returns false when the
   * capsule was refused or the write failed, which keeps the caller's legacy
   * fallback honest instead of reporting a delivery that did not happen.
   */
  fun write(body: String): Boolean {
    return try {
      val bytes = body.toByteArray(Charsets.UTF_8)
      if (bytes.size > MAX_FILE_BYTES) return false
      if (!directory.isDirectory && !directory.mkdirs()) return false
      prune(incomingBytes = bytes.size.toLong())
      val stamp = System.currentTimeMillis()
      val temp = File(directory, "$stamp.tmp")
      temp.writeBytes(bytes)
      // Atomic publish: a reader never sees a half-written capsule, and a
      // process killed mid-write leaves only the .tmp, which prune() reaps.
      val target = File(directory, "$stamp$SUFFIX")
      if (temp.renameTo(target)) true
      else {
        temp.delete()
        false
      }
    } catch (_: Throwable) {
      false
    }
  }

  /**
   * Capsules waiting to be uploaded, oldest first. Each is CLAIMED by rename so
   * a concurrent drain cannot take the same one; an unclaimed file stays for
   * the next launch.
   */
  fun claimPending(): List<File> {
    // Recover claims orphaned by a process that died mid-upload. Without this a
    // capsule claimed by a launch that crashed again before its POST finished
    // would sit unclaimable until the next capsule was written. Measured on
    // device: one such loss in the first post-fix run.
    recoverOrphanedClaims()
    val spooled =
      directory
        .listFiles { file -> file.isFile && file.name.endsWith(SUFFIX) }
        ?.sortedBy { it.name }
        ?: return emptyList()
    val claimed = ArrayList<File>(spooled.size)
    for (file in spooled) {
      val claim = File(directory, file.name.removeSuffix(SUFFIX) + CLAIM_SUFFIX)
      if (file.renameTo(claim)) claimed.add(claim)
    }
    return claimed
  }

  /** Return claims left by a process killed mid-upload to the pending set. */
  private fun recoverOrphanedClaims() {
    val files = directory.listFiles() ?: return
    for (file in files) {
      if (!file.name.endsWith(CLAIM_SUFFIX)) continue
      val name = file.name.removeSuffix(CLAIM_SUFFIX) + SUFFIX
      if (!file.renameTo(File(directory, name))) file.delete()
    }
  }

  /** Delete an uploaded capsule. Called only after the POST was accepted. */
  fun release(file: File) {
    try {
      file.delete()
    } catch (_: Throwable) {}
  }

  /**
   * Return a claimed capsule to the spool after a failed upload, so a network
   * outage defers delivery instead of destroying evidence.
   */
  fun restore(file: File) {
    try {
      val name = file.name.removeSuffix(CLAIM_SUFFIX) + SUFFIX
      if (!file.renameTo(File(directory, name))) file.delete()
    } catch (_: Throwable) {}
  }

  /**
   * Enforce the bounds and reap leftovers: stale `.tmp` files from a process
   * killed mid-write, and claims from a process killed mid-upload (those are
   * returned to the spool rather than dropped).
   */
  private fun prune(incomingBytes: Long) {
    val files = directory.listFiles() ?: return
    for (file in files) {
      if (file.name.endsWith(".tmp")) file.delete()
      if (file.name.endsWith(CLAIM_SUFFIX)) {
        val name = file.name.removeSuffix(CLAIM_SUFFIX) + SUFFIX
        if (!file.renameTo(File(directory, name))) file.delete()
      }
    }
    var spooled =
      (directory.listFiles { file -> file.isFile && file.name.endsWith(SUFFIX) } ?: return)
        .sortedBy { it.name }
        .toMutableList()
    // Oldest first: a newer capsule describes the failure the developer is
    // most likely still looking at.
    while (spooled.size + 1 > MAX_FILES && spooled.isNotEmpty()) {
      spooled.removeAt(0).delete()
    }
    var total = spooled.sumOf { it.length() }
    while (total + incomingBytes > MAX_TOTAL_BYTES && spooled.isNotEmpty()) {
      val oldest = spooled.removeAt(0)
      total -= oldest.length()
      oldest.delete()
    }
  }
}
