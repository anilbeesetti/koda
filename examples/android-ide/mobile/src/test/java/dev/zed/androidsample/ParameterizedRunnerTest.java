package dev.zed.androidsample;

import java.util.Arrays;
import java.util.Collection;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.junit.runners.Parameterized;
import static org.junit.Assert.assertEquals;

@RunWith(Parameterized.class)
public class ParameterizedRunnerTest {
    @Parameterized.Parameters
    public static Collection<Object[]> values() {
        return Arrays.asList(new Object[][] {{1, 2}, {2, 4}});
    }

    private final int input;
    private final int expected;

    public ParameterizedRunnerTest(int input, int expected) {
        this.input = input;
        this.expected = expected;
    }

    @Test public void doublesInput() {
        assertEquals(expected, input * 2);
    }
}
