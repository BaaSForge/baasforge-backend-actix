ALTER TABLE project_releases
    ALTER COLUMN revision TYPE INTEGER USING revision::integer;
