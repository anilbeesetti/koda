package app;

public class Values {
    public int field = 7;
    public int[] large = new int[2001];
    public Unloaded untouched = null;
    public Values() { large[2000] = 42; }
    public void inspect(int[] values) {
        int doubled = doubleValue(values[0]);
        System.out.println(doubled + field);
        try {
            throw new IllegalStateException("debug-fixture-caught");
        } catch (IllegalStateException expected) {
            System.out.println(expected.getMessage());
        }
    }
    public int doubleValue(int value) {
        return value * 2;
    }
}
class Unloaded {}
