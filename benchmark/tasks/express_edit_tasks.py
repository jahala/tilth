from tasks.base import Task, GroundTruth, Mutation


class ExpressEditJsonContentTypeTask(Task):
    """String literal change: res.json sets text/plain instead of application/json."""

    @property
    def name(self) -> str:
        return "express_edit_json_type"

    @property
    def repo(self) -> str:
        return "express"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            Mutation(
                file_path="lib/response.js",
                original="this.set('Content-Type', 'application/json');",
                mutated="this.set('Content-Type', 'text/plain');",
            )
        ]

    @property
    def test_command(self) -> list[str]:
        return ["npx", "mocha", "--require", "test/support/env", "--reporter",
                "spec", "--check-leaks", "--grep",
                "should respond with json for null", "test/res.json.js"]

    @property
    def prompt(self) -> str:
        return (
            "In Express's lib/response.js, the res.json() method is setting "
            "the wrong Content-Type header. JSON responses are being sent with "
            "an incorrect MIME type, causing clients that check Content-Type "
            "to fail parsing the response body as JSON. Find where res.json() "
            "sets the Content-Type and fix the MIME type."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()


class ExpressEditCookiePrefixTask(Task):
    """String literal change: JSON cookie prefix 'j:' becomes 'x:'."""

    @property
    def name(self) -> str:
        return "express_edit_cookie_prefix"

    @property
    def repo(self) -> str:
        return "express"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            Mutation(
                file_path="lib/response.js",
                original="? 'j:' + JSON.stringify(value)",
                mutated="? 'x:' + JSON.stringify(value)",
            )
        ]

    @property
    def test_command(self) -> list[str]:
        return ["npx", "mocha", "--require", "test/support/env", "--reporter",
                "spec", "--check-leaks", "--grep",
                "should generate a JSON cookie", "test/res.cookie.js"]

    @property
    def prompt(self) -> str:
        return (
            "In Express's lib/response.js, JSON cookie serialization is broken. "
            "When res.cookie() is called with an object value, it should prefix "
            "the serialized JSON with a standard marker so the cookie parser can "
            "identify it as JSON on the way back. The prefix is wrong, causing "
            "the cookie parser to treat JSON cookies as plain strings. Find the "
            "incorrect prefix in res.cookie() and fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()


class ExpressEditSendHtmlTypeTask(Task):
    """String literal change: res.send defaults to json instead of html for strings."""

    @property
    def name(self) -> str:
        return "express_edit_send_type"

    @property
    def repo(self) -> str:
        return "express"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            Mutation(
                file_path="lib/response.js",
                original="this.type('html');",
                mutated="this.type('json');",
            )
        ]

    @property
    def test_command(self) -> list[str]:
        return ["npx", "mocha", "--require", "test/support/env", "--reporter",
                "spec", "--check-leaks", "--grep",
                "should send as html", "test/res.send.js"]

    @property
    def prompt(self) -> str:
        return (
            "In Express's lib/response.js, the res.send() method is setting "
            "the wrong default Content-Type for string responses. When you call "
            "res.send('<p>hello</p>'), the response should have Content-Type "
            "text/html, but it's being sent as a different type. The issue is "
            "in the string branch of res.send() where it sets the default type. "
            "Find and fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
