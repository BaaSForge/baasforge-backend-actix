#!/bin/bash
docker exec -it postgres_db psql -U baal -d baal_db -c "SELECT pid, usename, application_name, client_addr, state FROM pg_stat_activity;"