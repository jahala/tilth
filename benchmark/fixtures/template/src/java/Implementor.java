interface Notifier {
    void notify(String message);
}

final class EmailNotifier implements Notifier {
    @Override
    public void notify(String message) {
        System.out.println("email:" + message);
    }
}

final class WebhookNotifier implements Notifier {
    @Override
    public void notify(String message) {
        System.out.println("webhook:" + message);
    }
}

public final class Implementor {
    public static Notifier defaultNotifier() {
        return new EmailNotifier();
    }
}
