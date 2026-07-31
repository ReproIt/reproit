package com.reproit.android

import java.io.File
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * The spool is what keeps a crash capsule alive across the process death that
 * used to eat it, so its bounds and its at-most-once delivery are contracts,
 * not implementation details.
 */
class CapsuleSpoolTest {
  private lateinit var dir: File

  @Before
  fun setUp() {
    dir = File(System.getProperty("java.io.tmpdir"), "reproit-spool-test-${System.nanoTime()}")
  }

  @After
  fun tearDown() {
    dir.deleteRecursively()
  }

  @Test
  fun `a written capsule is claimable and released only once`() {
    val spool = CapsuleSpool(dir)
    assertTrue(spool.write("""{"batchId":"one"}"""))

    val claimed = spool.claimPending()
    assertEquals(1, claimed.size)
    assertEquals("""{"batchId":"one"}""", claimed[0].readText())

    // Released after an accepted upload, the capsule is gone for good, so an
    // accepted capsule is never posted twice. (A claim NOT released, because
    // the process died mid-upload, is deliberately recoverable by a later
    // drain; that is the separate orphan-recovery contract below. The SDK
    // drains once per launch, so the two cannot collide within a process.)
    spool.release(claimed[0])
    assertTrue(CapsuleSpool(dir).claimPending().isEmpty())
  }

  @Test
  fun `a failed upload restores the capsule for the next launch`() {
    val spool = CapsuleSpool(dir)
    spool.write("""{"batchId":"deferred"}""")
    val claimed = spool.claimPending().single()

    spool.restore(claimed)

    val again = CapsuleSpool(dir).claimPending()
    assertEquals(1, again.size)
    assertEquals("""{"batchId":"deferred"}""", again[0].readText())
  }

  @Test
  fun `the file count is bounded and the oldest is dropped`() {
    val spool = CapsuleSpool(dir)
    for (i in 1..CapsuleSpool.MAX_FILES + 3) {
      assertTrue(spool.write("""{"batchId":"$i"}"""))
      // The name carries a millisecond stamp, so distinct writes need distinct
      // milliseconds for the oldest-first ordering to mean anything.
      Thread.sleep(2)
    }
    val pending = spool.claimPending()
    assertTrue("spool kept ${pending.size} files", pending.size <= CapsuleSpool.MAX_FILES)
    // The newest survived: it describes the failure most likely being chased.
    assertTrue(pending.any { it.readText().contains("\"${CapsuleSpool.MAX_FILES + 3}\"") })
  }

  @Test
  fun `an oversized capsule is refused rather than truncated`() {
    val spool = CapsuleSpool(dir)
    val huge = "x".repeat((CapsuleSpool.MAX_FILE_BYTES + 1).toInt())
    assertFalse(spool.write(huge))
    assertTrue(spool.claimPending().isEmpty())
  }

  @Test
  fun `total bytes stay under the cap`() {
    val spool = CapsuleSpool(dir)
    val chunk = "y".repeat(200 * 1024)
    repeat(12) {
      spool.write(chunk)
      Thread.sleep(2)
    }
    val total = spool.claimPending().sumOf { it.length() }
    assertTrue("spool held $total bytes", total <= CapsuleSpool.MAX_TOTAL_BYTES)
  }

  @Test
  fun `a claim orphaned by a killed process is recovered on the next drain`() {
    val spool = CapsuleSpool(dir)
    spool.write("""{"batchId":"orphan"}""")
    val claimed = spool.claimPending().single()
    // The process dies here: the claim is never released and never restored.
    assertTrue(claimed.exists())

    // A fresh launch must find it again, without needing a new capsule first.
    val recovered = CapsuleSpool(dir).claimPending()
    assertEquals(1, recovered.size)
    assertEquals("""{"batchId":"orphan"}""", recovered[0].readText())
  }

  @Test
  fun `a temp file left by a killed process is reaped`() {
    dir.mkdirs()
    File(dir, "999.tmp").writeText("half written")
    val spool = CapsuleSpool(dir)
    spool.write("""{"batchId":"after"}""")
    assertTrue(dir.listFiles()!!.none { it.name.endsWith(".tmp") })
  }
}
