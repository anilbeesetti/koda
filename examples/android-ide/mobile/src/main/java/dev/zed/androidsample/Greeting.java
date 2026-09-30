package dev.zed.androidsample;

public final class Greeting {
    private Greeting() {}

    public static String message(String edition) {
        return "Hello from the " + edition + " variant!";
    }
}
