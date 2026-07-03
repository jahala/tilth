from tasks.base import Task, GroundTruth


class ExpressJsonSendTask(Task):
    @property
    def name(self) -> str:
        return "express_json_send"

    @property
    def repo(self) -> str:
        return "express"

    @property
    def prompt(self) -> str:
        return (
            "In Express, trace how res.json() works internally. Show the implementation "
            "of both res.json and res.send. How does res.send handle different content types "
            "(strings, buffers, objects)? What application settings does res.json read?"
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["res.json", "json replacer", "json spaces", "res.send", "Content-Type"],
        )


class ExpressRenderChainTask(Task):
    @property
    def name(self) -> str:
        return "express_render_chain"

    @property
    def repo(self) -> str:
        return "express"

    @property
    def prompt(self) -> str:
        return (
            "Trace the full template rendering chain in Express. When res.render() is called, "
            "how does it reach the template engine? Show the code path through res.render, "
            "app.render, and the View class. How does View resolve the template file path?"
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["res.render", "app.render", "View", "lookup"],
        )

    @property
    def task_type(self) -> str:
        return "navigate"


class ExpressAppInitTask(Task):
    @property
    def name(self) -> str:
        return "express_app_init"

    @property
    def repo(self) -> str:
        return "express"

    @property
    def prompt(self) -> str:
        return (
            "How does Express create and initialize an application? Show what happens when "
            "you call express(). Find the createApplication function and trace through "
            "app.init. What default settings are configured? Where is the router created?"
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["createApplication", "express.js", "application.js", "trust proxy", "etag"],
        )

    @property
    def task_type(self) -> str:
        return "navigate"


class ExpressResSendTask(Task):
    @property
    def name(self) -> str:
        return "express_res_send"

    @property
    def repo(self) -> str:
        return "express"

    @property
    def prompt(self) -> str:
        return (
            "Find the res.send function implementation in Express. Show its "
            "complete code."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["res.send", "response", "Content-Length"],
        )


class ExpressAppRenderTask(Task):
    @property
    def name(self) -> str:
        return "express_app_render"

    @property
    def repo(self) -> str:
        return "express"

    @property
    def prompt(self) -> str:
        return (
            "Find app.render in Express's application.js. Show its "
            "implementation, then find the View class it uses and explain "
            "how View resolves template file paths."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "app.render", "View", "application.js",
            ],
        )

    @property
    def task_type(self) -> str:
        return "navigate"
