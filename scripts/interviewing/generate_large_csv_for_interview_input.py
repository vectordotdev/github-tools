import csv
import random
import time
from datetime import datetime, timedelta

# Sample data pools
hosts = ["10.0.0.2", "10.0.0.4", "192.168.1.1", "172.16.0.5", "8.8.8.8"]
methods = ["GET", "HEAD", "OPTIONS", "TRACE", "PUT", "DELETE", "POST", "PATCH", "CONNECT"]
paths = ["/api/user", "/api/help", "/login", "/logout", "/data", "/admin"]
protocols = ["HTTP/1.0", "HTTP/1.1", "HTTP/2.0"]
statuses = [200, 201, 400, 401, 403, 404, 500, 503]

# Base timestamp: 1 year ago from today (March 20, 2025)
base_time = datetime(2024, 3, 20)
current_time = datetime(2025, 3, 20)

# Open file to write CSV
with open("../config/stress_test.csv", "w", newline="") as f:
    writer = csv.writer(f, quoting=csv.QUOTE_ALL)
    # Write header
    writer.writerow(["remotehost", "rfc931", "authuser", "date", "request", "status", "bytes"])

    # Generate 1 million rows
    for _ in range(1_000_000):
        host = random.choice(hosts)
        rfc931 = "-" if random.random() < 0.95 else "user" + str(random.randint(1, 100))  # Rare non-empty rfc931
        authuser = "apache" if random.random() < 0.9 else random.choice(["nginx", "admin", "guest"])

        # Realistic timestamp: Random time in the last year
        delta = timedelta(seconds=random.randint(0, int((current_time - base_time).total_seconds())))
        timestamp = int((base_time + delta).timestamp())

        # Request with occasional long outlier
        method = random.choice(methods)
        path = random.choice(paths)
        protocol = random.choice(protocols)
        if random.random() < 0.01:  # 1% chance of outlier
            path = "/api/" + ("x" * random.randint(1000, 10000))  # Massive request string
        request = f"{method} {path} {protocol}"

        status = random.choice(statuses)

        # Bytes with occasional outlier
        if random.random() < 0.005:  # 0.5% chance of huge bytes
            bytes_ = random.randint(1_000_000, 10_000_000)  # Outlier: 1MB-10MB
        else:
            bytes_ = random.randint(100, 10_000)  # Normal range

        writer.writerow([host, rfc931, authuser, timestamp, request, status, bytes_])

print("Generated stress_test.csv with 1 million rows, outliers, and realistic timestamps.")
