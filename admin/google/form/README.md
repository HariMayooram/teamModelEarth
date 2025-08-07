# Member Registration Form

A comprehensive Google Sheets-integrated member management form with OAuth authentication and smart data pre-loading.

## Features

- ✅ **Google OAuth Authentication** - Secure sign-in with Google accounts
- ✅ **Smart Form Pre-loading** - Automatically loads existing member data for editing
- ✅ **Google Sheets Integration** - Direct read/write to member management spreadsheet
- ✅ **Progressive Form Validation** - Real-time progress tracking and validation
- ✅ **Interactive Elements** - Job title autocomplete, team checkboxes, UN Goal selection
- ✅ **Responsive Design** - Works on desktop and mobile devices
- ✅ **Duplicate Handling** - Updates most recent row if multiple emails exist
- ✅ **Preview Functionality** - Review data before submission

## Setup Instructions

### 1. Google Cloud Console Setup

1. **Create or Select a Google Cloud Project**
   - Go to [Google Cloud Console](https://console.cloud.google.com)
   - Create a new project or select existing one
   - Note the Project ID for configuration

2. **Enable Required APIs**
   - Google Sheets API
   - Google Identity Services (for OAuth)
   - Go to APIs & Services > Library and enable these APIs

3. **Create Service Account** (for Sheets API)
   - Go to IAM & Admin > Service Accounts
   - Click "Create Service Account"
   - Name: `google-sheets-service`
   - Grant role: `Editor` (for sheet access)
   - Create and download JSON key file

4. **Create OAuth 2.0 Client** (for user authentication)
   - Go to APIs & Services > Credentials
   - Click "Create Credentials" > "OAuth 2.0 Client ID"
   - Application type: Web application
   - Authorized JavaScript origins: `http://localhost:8887`
   - Note the Client ID for configuration

### 2. Google Sheets Setup

1. **Create Member Management Sheet**
   - Create a new Google Sheet
   - Name it "PartnerTools Members" or similar
   - Set up headers in row 1 (see column mapping below)
   - Share the sheet with your service account email (found in the JSON key file)
   - Give "Editor" permissions
   - Note the Spreadsheet ID from the URL

2. **Required Columns** (in order):
   ```
   Timestamp | Handle | Name | ToDos | Projects | Team | Focus | UN Goal | 
   School and Degree Program | Your Location | Github | Status | Email | 
   StartDate | EndDate | Note | Your Website | Date Degree Completed | 
   OPT University Department | OPT University Department,Email Phone | 
   HoursPerWeek | Job Title | Phone
   ```

### 3. Environment Configuration

Update your `.env` file with the following variables:

```bash
# Google OAuth Configuration
GOOGLE_OAUTH_CLIENT_ID=your_oauth_client_id_here

# Google Sheets Configuration  
GOOGLE_SHEETS_ID=your_google_sheet_id_here
GOOGLE_SERVICE_KEY=/path/to/your/service-account-key.json

# Or inline JSON (not recommended for production)
GOOGLE_SERVICE_KEY_JSON={"type":"service_account","project_id":"..."}
```

### 4. Form Configuration

Update `config.json` with your specific settings:

```json
{
  "googleSheets": {
    "spreadsheetId": "YOUR_GOOGLE_SHEET_ID",
    "worksheetName": "Members",
    "headerRow": 1,
    "dataStartRow": 2
  },
  "oauth": {
    "clientId": "YOUR_GOOGLE_OAUTH_CLIENT_ID"
  }
}
```

### 5. Frontend Configuration

Update the Google Client ID in `index.html`:

```html
<div id="g_id_onload"
     data-client_id="YOUR_GOOGLE_CLIENT_ID"
     ...>
</div>
```

And in `form.js`:
```javascript
google.accounts.id.initialize({
    client_id: 'YOUR_GOOGLE_CLIENT_ID',
    callback: handleCredentialResponse
});
```

## API Endpoints

The form integrates with these backend endpoints:

- `GET /api/google/sheets/config` - Get form configuration
- `POST /api/google/auth/verify` - Verify Google OAuth token
- `GET /api/google/sheets/member/{email}` - Get existing member data
- `POST /api/google/sheets/member` - Create new member
- `PUT /api/google/sheets/member` - Update existing member

## Usage

1. **Access the Form**
   - Navigate to `/admin/google/form/`
   - Sign in with your Google account

2. **Fill Out Registration**
   - Complete required fields (marked with *)
   - Use autocomplete for job titles
   - Select multiple teams and UN Goals as applicable
   - Progress indicator shows completion status

3. **Submit or Update**
   - Preview your data before submitting
   - If email exists in sheet, form will pre-load existing data
   - Submit to create new record or update existing

## Form Fields

### Required Fields
- **Name** - First and last name
- **Team** - Programming teams of interest
- **UN Goal** - Areas for contribution  
- **Location** - City, State where participating
- **Status** - New/Existing/Returning
- **GitHub** - GitHub username
- **Start Date** - Participation start date
- **Job Title** - Position for welcome letter

### Optional Fields
- **Handle** - Display name/identifier
- **Focus** - Specific projects of interest
- **School** - Educational background
- **Phone** - Contact number
- **Website** - Portfolio/resume link
- **Projects** - Current/planned projects
- **ToDos** - Tasks and goals
- **Note** - Additional information

### OPT Student Fields
- **Date Degree Completed**
- **OPT University Department**
- **Department Contact Info**
- **Hours Per Week** (22 hours meets OPT requirements)

## Technical Features

### Smart Data Handling
- **Email Matching**: Automatically finds existing records by email
- **Duplicate Resolution**: Updates most recent row if multiple emails found
- **Data Validation**: Client and server-side validation
- **Progress Tracking**: Real-time completion percentage

### Interactive Elements
- **Job Title Autocomplete**: Suggests common titles
- **Team Selection**: Multiple checkbox selection with "Other" option
- **UN Goals**: Comprehensive list of sustainable development areas
- **Status Dropdown**: Predefined status options

### Security
- **OAuth Authentication**: Secure Google sign-in
- **Service Account**: Separate credentials for sheet access
- **Input Validation**: Prevents malicious data
- **CORS Configuration**: Proper cross-origin request handling

## Troubleshooting

### Common Issues

1. **"OAuth verification not implemented" error**
   - Backend Google OAuth integration needs to be completed
   - Use manual data entry for now

2. **"Sheets API integration not implemented" error**
   - Google Sheets API integration needs to be completed
   - Form will work but won't save to sheets yet

3. **Google Sign-in doesn't appear**
   - Check that Google Client ID is correctly configured
   - Verify JavaScript origins in Google Cloud Console

4. **Sheet access denied**
   - Ensure service account email has Editor access to the sheet
   - Verify sheet ID in configuration

### Next Steps for Full Implementation

1. **Complete Google OAuth Backend Integration**
   - JWT token verification
   - User profile extraction
   - Session management

2. **Complete Google Sheets API Integration**
   - Service account authentication
   - Read/write operations
   - Error handling and retries

3. **Add Email Notifications** (Optional)
   - Welcome email for new members
   - Confirmation emails for updates

4. **Enhanced Security** (Production)
   - Rate limiting
   - Input sanitization
   - Access logging

## Files Structure

```
admin/google/form/
├── index.html          # Main form interface
├── form.js            # JavaScript functionality
├── config.json        # Configuration settings
└── README.md          # This documentation
```

## Dependencies

- **Frontend**: Google Identity Services, Feather Icons, Modern CSS Grid
- **Backend**: Actix-web, Google Sheets API, OAuth2 verification
- **Storage**: Google Sheets, Environment variables

For support or questions, see the main project documentation or contact the development team.