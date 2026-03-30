ALTER TABLE project_releases
    ALTER COLUMN revision TYPE DOUBLE PRECISION USING revision::double precision;
