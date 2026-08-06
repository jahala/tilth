# Flask API Service

A production-ready Flask-based REST API service with authentication, rate limiting, and database connection pooling.

## Features

- JWT-based authentication with access and refresh tokens
- Rate limiting on API endpoints
- Thread-safe database connection pooling
- Input validation and error handling
- Structured logging
- Comprehensive test coverage
- RESTful API design

## Table of Contents

- [Installation](#installation)
- [Usage](#usage)
- [API Reference](#api-reference)
- [Configuration](#configuration)
- [Deployment](#deployment)
- [Development](#development)
- [Testing](#testing)
- [Contributing](#contributing)

## Installation

### Prerequisites

- Python 3.9 or higher
- PostgreSQL 12 or higher
- Redis (optional, for distributed rate limiting)

### Setup

1. Clone the repository:

```bash
git clone https://github.com/yourorg/flask-api.git
cd flask-api
```

2. Create a virtual environment:

```bash
python -m venv venv
source venv/bin/activate  # On Windows: venv\Scripts\activate
```

3. Install dependencies:

```bash
pip install -e .
```

4. Set up the database:

```bash
createdb flask_api_dev
python -m src.database.migrations run
```

5. Configure environment variables:

```bash
cp .env.example .env
# Edit .env with your configuration
```

## Usage

### Running the development server

```bash
flask run --debug
```

The API will be available at http://localhost:5000

### Running in production

```bash
gunicorn -w 4 -b 0.0.0.0:8000 "src.app:create_app()"
```

## API Reference

### Authentication Endpoints

#### POST /api/auth/login

Authenticate a user and receive access and refresh tokens.

**Request Body:**
```json
{
  "email": "user@example.com",
  "password": "securepassword123"
}
```

**Response:**
```json
{
  "access_token": "eyJhbGc...",
  "refresh_token": "eyJhbGc...",
  "token_type": "bearer",
  "expires_in": 1800
}
```

#### POST /api/auth/refresh

Refresh an expired access token using a refresh token.

**Request Body:**
```json
{
  "refresh_token": "eyJhbGc..."
}
```

**Response:**
```json
{
  "access_token": "eyJhbGc...",
  "token_type": "bearer",
  "expires_in": 1800
}
```

### User Endpoints

#### GET /api/users/me

Get the current authenticated user's profile.

**Headers:**
```
Authorization: Bearer <access_token>
```

**Response:**
```json
{
  "id": "123",
  "email": "user@example.com",
  "name": "John Doe",
  "created_at": "2026-01-15T10:30:00Z"
}
```

#### PATCH /api/users/me

Update the current user's profile.

**Headers:**
```
Authorization: Bearer <access_token>
```

**Request Body:**
```json
{
  "name": "Jane Doe",
  "phone": "+1234567890"
}
```

### Order Endpoints

#### POST /api/orders

Create a new order.

**Headers:**
```
Authorization: Bearer <access_token>
```

**Request Body:**
```json
{
  "items": [
    {"product_id": "abc", "quantity": 2},
    {"product_id": "def", "quantity": 1}
  ],
  "shipping_address": {
    "street": "123 Main St",
    "city": "San Francisco",
    "state": "CA",
    "zip": "94102"
  }
}
```

#### GET /api/orders/{order_id}

Get details of a specific order.

**Headers:**
```
Authorization: Bearer <access_token>
```

**Response:**
```json
{
  "id": "order_123",
  "user_id": "user_456",
  "items": [...],
  "status": "pending",
  "created_at": "2026-02-10T14:30:00Z"
}
```

## Configuration

The application can be configured using environment variables or a configuration file.

### Environment Variables

- `FLASK_ENV`: Application environment (development, production, testing)
- `DEBUG`: Enable debug mode (true/false)
- `SECRET_KEY`: Secret key for JWT signing
- `DATABASE_URL`: PostgreSQL connection string
- `REDIS_URL`: Redis connection string (optional)
- `LOG_LEVEL`: Logging level (DEBUG, INFO, WARNING, ERROR)

### Configuration File

Create a `config.py` file in the project root:

```python
class Config:
    SECRET_KEY = "your-secret-key"
    SQLALCHEMY_DATABASE_URI = "postgresql://user:pass@localhost/dbname"
    JWT_ALGORITHM = "HS256"
    ACCESS_TOKEN_EXPIRE_MINUTES = 30
```

## Deployment

### Docker Deployment

Build the Docker image:

```bash
docker build -t flask-api:latest .
```

Run the container:

```bash
docker run -d \
  -p 8000:8000 \
  -e DATABASE_URL=postgresql://user:pass@host/db \
  -e SECRET_KEY=your-secret-key \
  -e REDIS_URL=redis://host:6379 \
  -e ALLOWED_HOSTS=api.example.com \
  -e LOG_LEVEL=INFO \
  flask-api:latest
```

### Kubernetes Deployment

Apply the Kubernetes manifests:

```bash
kubectl apply -f k8s/deployment.yaml
kubectl apply -f k8s/service.yaml
kubectl apply -f k8s/ingress.yaml
```

### Environment Variables for Deployment

The following environment variables must be configured in your deployment environment:

- `DATABASE_URL`: PostgreSQL connection string (required)
- `SECRET_KEY`: JWT signing secret (required)
- `REDIS_URL`: Redis connection string for distributed rate limiting (optional)
- `ALLOWED_HOSTS`: Comma-separated list of allowed host headers (required in production)
- `LOG_LEVEL`: Logging level (DEBUG, INFO, WARNING, ERROR, CRITICAL)

### Health Checks

The API provides a health check endpoint at `/api/health` that returns:

```json
{
  "status": "healthy",
  "timestamp": "2026-02-12T10:30:00Z",
  "version": "1.0.0"
}
```

## Development

### Project Structure

```
flask-api/
├── src/
│   ├── api/          # API routes and handlers
│   ├── auth/         # Authentication logic
│   ├── database/     # Database connection and queries
│   ├── models/       # Data models
│   └── utils/        # Utility functions
├── tests/            # Test files
├── docs/             # Documentation
└── pyproject.toml    # Project configuration
```

### Code Style

This project follows PEP 8 style guidelines. Use `black` for code formatting:

```bash
black src/ tests/
```

Run linting with `ruff`:

```bash
ruff check src/ tests/
```

## Testing

Run the test suite:

```bash
pytest
```

Run with coverage:

```bash
pytest --cov=src --cov-report=html
```

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## License

This project is licensed under the MIT License - see the LICENSE file for details.
