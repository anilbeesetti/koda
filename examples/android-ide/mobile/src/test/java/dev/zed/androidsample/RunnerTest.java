package dev.zed.androidsample;

import org.junit.Ignore;
import org.junit.Test;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertEquals;

public class RunnerTest {
    @Test public void passes() {
        assertEquals(4, 2 + 2);
    }

    @Ignore("Intentional skipped test for the runner fixture")
    @Test public void skipped() {}

    @Test public void optionalFailure() {
        assertFalse("Intentional source-linked fixture failure", Boolean.getBoolean("koda.fixture.fail"));
    }
}
