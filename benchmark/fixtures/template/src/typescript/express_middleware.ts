export type Request = { headers: Record<string, string> };
export type Response = { statusCode: number };
export type Next = () => void;
export type RequestHandler = (request: Request, response: Response, next: Next) => void;

export function createAuthMiddleware(requiredRole: string): RequestHandler {
    return (_request, response, next) => {
        response.statusCode = requiredRole ? 200 : 403;
        next();
    };
}

export function registerAuth(
    app: { use(handler: RequestHandler): void },
    requiredRole: string,
): void {
    app.use(createAuthMiddleware(requiredRole));
}
