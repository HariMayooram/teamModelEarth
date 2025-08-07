// Configuration file for PartnerTools
// Copy this file to settings.js and update with your actual values
// Database and Gemini Config are in .env

/*
provides frontend configuration including:
    - API Base URL (CONFIG.API.BASE_URL: 'http://localhost:8081/api')
    - OAuth provider settings (Google, GitHub, LinkedIn)
    - Gemini AI configuration
    - Feature flags and UI settings
    - Error messages and validation rules
*/

const CONFIG = {
    // API Configuration
    API: {
        BASE_URL: 'http://localhost:8081/api',
        TIMEOUT: 10000, // 10 seconds
    },

    // Authentication Configuration
    AUTH: {
        // OAuth Provider Settings (add your actual keys)
        GOOGLE: {
            CLIENT_ID: 'your-google-client-id.apps.googleusercontent.com',
            REDIRECT_URI: window.location.origin + '/auth/google/callback'
        },
        
        GITHUB: {
            CLIENT_ID: 'your-github-client-id',
            REDIRECT_URI: window.location.origin + '/auth/github/callback'
        },
        
        LINKEDIN: {
            CLIENT_ID: 'your-linkedin-client-id',
            REDIRECT_URI: window.location.origin + '/auth/linkedin/callback'
        },
        
        // JWT Settings
        JWT_SECRET: 'your-jwt-secret-key-change-this-in-production',
        TOKEN_EXPIRY: '24h'
    },

    // Gemini AI Configuration
    GEMINI: {
        API_KEY: 'your-gemini-api-key-here',
        MODEL: 'gemini-pro',
        TEMPERATURE: 0.7,
        MAX_TOKENS: 1000
    },

    // Feature Flags
    FEATURES: {
        GEMINI_AI_SEARCH: true,
        SMART_INSIGHTS: true,
        SURVEY_ANALYTICS: true,
        REAL_TIME_NOTIFICATIONS: false,
        ADVANCED_FILTERING: true,
        COLLABORATION_TOOLS: true
    },

    // Application Settings
    APP: {
        NAME: 'PartnerTools',
        VERSION: '1.0.0',
        ENVIRONMENT: 'development', // development, staging, production
        DEBUG: true,
        
        // UI Settings
        DEFAULT_THEME: 'light',
        SIDEBAR_COLLAPSED: false,
        ITEMS_PER_PAGE: 20,
        
        // Survey Settings
        SURVEY_QUESTIONS_COUNT: 20,
        INTERESTS_COUNT: 20,
        AUTO_SAVE_INTERVAL: 30000 // 30 seconds
    },

    // External Services
    SERVICES: {
        // Email Service (for notifications)
        EMAIL: {
            PROVIDER: 'smtp', // smtp, sendgrid, etc.
            FROM_ADDRESS: 'noreply@yourdomain.com'
        },
        
        // Analytics
        ANALYTICS: {
            ENABLED: false,
            GOOGLE_ANALYTICS_ID: 'GA-XXXXXXXXX'
        },
        
        // File Storage
        STORAGE: {
            PROVIDER: 'local', // local, aws-s3, google-cloud
            UPLOAD_LIMIT: '10MB'
        }
    },

    // Google Meetup Configuration
    MEETUP: {
        SPREADSHEET_ID: 'your-google-spreadsheet-id'
    },

    // Validation Rules
    VALIDATION: {
        PASSWORD_MIN_LENGTH: 8,
        USERNAME_MIN_LENGTH: 3,
        PROJECT_NAME_MAX_LENGTH: 100,
        DESCRIPTION_MAX_LENGTH: 2000
    },

    // Error Messages
    MESSAGES: {
        ERRORS: {
            NETWORK_ERROR: 'Unable to connect to server. Please check your internet connection.',
            AUTH_FAILED: 'Authentication failed. Please try again.',
            PERMISSION_DENIED: 'You do not have permission to perform this action.',
            VALIDATION_ERROR: 'Please check your input and try again.',
            SERVER_ERROR: 'An unexpected error occurred. Please try again later.'
        },
        
        SUCCESS: {
            PROJECT_CREATED: 'Project created successfully!',
            PROFILE_UPDATED: 'Profile updated successfully!',
            SURVEY_COMPLETED: 'Survey completed successfully!',
            TEAM_JOINED: 'Successfully joined the team!'
        }
    }
};

// Export configuration
if (typeof module !== 'undefined' && module.exports) {
    module.exports = CONFIG;
} else {
    window.CONFIG = CONFIG;
}