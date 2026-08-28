import java.io.File
import kotlin.test.Test
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import org.junit.Rule
import org.junit.rules.TemporaryFolder

class CargoJniLibsTest {
    @get:Rule
    val directory = TemporaryFolder()

    @Test
    fun `a library built for the target is accepted`() {
        checkBuiltFor(library(elfHeader(bits = 64, machine = AARCH64)), "aarch64-linux-android")
    }

    @Test
    fun `a 32-bit library is rejected, whatever its architecture`() {
        val failure = assertFailsWith<IllegalStateException> {
            checkBuiltFor(library(elfHeader(bits = 32, machine = AARCH64)), "aarch64-linux-android")
        }

        assertTrue(failure.message.orEmpty().contains("32-bit"))
    }

    @Test
    fun `a library built for another architecture is rejected`() {
        val failure = assertFailsWith<IllegalStateException> {
            checkBuiltFor(library(elfHeader(bits = 64, machine = AARCH64)), "x86_64-linux-android")
        }

        assertTrue(failure.message.orEmpty().contains("x86_64-linux-android"))
    }

    @Test
    fun `something that is not an ELF file at all is rejected`() {
        assertFailsWith<IllegalStateException> {
            checkBuiltFor(library(ByteArray(64)), "aarch64-linux-android")
        }
    }

    @Test
    fun `a truncated file is rejected rather than read past its end`() {
        assertFailsWith<IllegalStateException> {
            checkBuiltFor(library(elfHeader(bits = 64, machine = AARCH64).copyOf(8)), "aarch64-linux-android")
        }
    }

    @Test
    fun `a library the cargo build never produced is reported`() {
        assertFailsWith<IllegalStateException> {
            checkBuiltFor(File(directory.root, "nothing.so"), "aarch64-linux-android")
        }
    }

    private fun library(content: ByteArray): File =
        directory.newFile("lib${content.size}.so").apply { writeBytes(content) }
}

private const val AARCH64 = 0xB7

private fun elfHeader(bits: Int, machine: Int) = ByteArray(64).apply {
    this[0] = 0x7F
    this[1] = 'E'.code.toByte()
    this[2] = 'L'.code.toByte()
    this[3] = 'F'.code.toByte()
    this[4] = if (bits == 64) 2 else 1
    this[5] = 1 // little endian
    this[18] = (machine and 0xFF).toByte()
    this[19] = (machine shr 8).toByte()
}
