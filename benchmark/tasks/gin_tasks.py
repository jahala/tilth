from tasks.base import Task, GroundTruth


class GinRadixTreeTask(Task):
    @property
    def name(self) -> str:
        return "gin_radix_tree"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def prompt(self) -> str:
        return (
            "Find the radix tree implementation that gin uses for routing. Show the node "
            "struct definition, the nodeType constants, and explain how wildcard parameters "
            "(like :id and *filepath) are handled in the tree. What method looks up a route?"
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["type node struct", "catchAll", "getValue", "wildChild"],
        )


class GinClientIPTask(Task):
    @property
    def name(self) -> str:
        return "gin_client_ip"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def prompt(self) -> str:
        return (
            "In gin's Context, show the complete implementation of the ClientIP() method. "
            "What headers does it check, in what order? How does it handle trusted proxies "
            "and the X-Forwarded-For header?"
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "func (c *Context) ClientIP",
                "RemoteIPHeaders",
                "X-Forwarded-For",
                "trustedCIDRs",
            ],
        )


class GinMiddlewareChainTask(Task):
    @property
    def name(self) -> str:
        return "gin_middleware_chain"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def prompt(self) -> str:
        return (
            "Trace how a request flows through gin's middleware chain. Start from "
            "Engine.ServeHTTP, show how it gets a Context from the pool, finds the route "
            "handlers, and executes the handler chain. What is HandlersChain and how does "
            "Context.Next() advance through it?"
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["ServeHTTP", "HandlersChain", "Next", "pool", "index"],
        )

    @property
    def task_type(self) -> str:
        return "navigate"


class GinContextNextTask(Task):
    @property
    def name(self) -> str:
        return "gin_context_next"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def prompt(self) -> str:
        return (
            "Find the Context.Next() method in gin. Show its complete "
            "implementation."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "Next", "index", "handlers",
            ],
        )


class GinServeHTTPFlowTask(Task):
    @property
    def name(self) -> str:
        return "gin_servehttp_flow"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def prompt(self) -> str:
        return (
            "Find Engine.ServeHTTP in gin and show its implementation. Then "
            "trace what functions it calls — how does it get a Context, find "
            "the matching route handlers, and start executing them?"
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "ServeHTTP", "gin.go", "handleHTTPRequest", "pool",
            ],
        )

    @property
    def task_type(self) -> str:
        return "navigate"
